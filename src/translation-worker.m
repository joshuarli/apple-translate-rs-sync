// translation-worker: standalone ObjC binary hosting EMTTranslator engines.
// Uses a count-based batch protocol over stdin/stdout so the worker stays
// alive across batches (no per-batch process startup).
//
// Protocol (stdin):
//   <count>\n        — number of texts in this batch (0 = exit)
//   <byte-len>\n     — UTF-8 byte length for the next source text
//   <bytes>          — source text bytes, repeated <count> times
//
// Protocol (stdout):
//   <byte-len>\n     — UTF-8 byte length for the next translated text
//   <bytes>          — translated text bytes, same order as inputs
//
// Usage: translation-worker <assets-dir> <num-engines> <src-lang> <tgt-lang>

#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#import <objc/message.h>
#import <dlfcn.h>
#import <errno.h>
#import <string.h>
#import <unistd.h>

static const char* safeUTF8(NSString *value) {
    const char *text = [value UTF8String];
    return text ?: "(unavailable)";
}

static NSString* fileSystemString(const char *path) {
    if (!path) return nil;
    return [[NSFileManager defaultManager] stringWithFileSystemRepresentation:path
                                                                       length:strlen(path)];
}

static bool parseLongLine(const char *line, long min, long max, long *out) {
    if (!line || !out) return false;

    errno = 0;
    char *end = NULL;
    long value = strtol(line, &end, 10);
    if (errno != 0 || end == line || value < min || value > max) {
        return false;
    }
    while (*end == ' ' || *end == '\t' || *end == '\r' || *end == '\n') {
        end++;
    }
    if (*end != '\0') {
        return false;
    }

    *out = value;
    return true;
}

static id createEngine(const char *assetsDir) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        dlopen("/System/Library/PrivateFrameworks/EmbeddedAcousticRecognition.framework/EmbeddedAcousticRecognition", RTLD_LAZY);
        dlopen("/System/Library/PrivateFrameworks/TranslationDaemon.framework/TranslationDaemon", RTLD_LAZY);
    });

    Class EMTCls = NSClassFromString(@"EMTTranslator");
    if (!EMTCls) return NULL;

    NSString *path = fileSystemString(assetsDir);
    if (!path) return NULL;

    NSURL *url = [NSURL fileURLWithPath:path];
    SEL initSel = NSSelectorFromString(@"initWithModelURL:task:skipNonFinalToCatchup:translatorCacheSize:useGlobalTranslationQueue:");

    id engine = [EMTCls alloc];
    NSMethodSignature *sig = [engine methodSignatureForSelector:initSel];
    if (!sig) return NULL;

    @try {
        NSInvocation *inv = [NSInvocation invocationWithMethodSignature:sig];
        [inv setTarget:engine];
        [inv setSelector:initSel];
        [inv setArgument:&url atIndex:2];
        NSString *task = @"mt_app";
        [inv setArgument:&task atIndex:3];
        bool skip = false;
        [inv setArgument:&skip atIndex:4];
        int cacheSize = 1;
        [inv setArgument:&cacheSize atIndex:5];
        bool useGlobal = false;
        [inv setArgument:&useGlobal atIndex:6];
        [inv invoke];

        __unsafe_unretained id result = nil;
        [inv getReturnValue:&result];

        if (result) {
            SEL setQueueSel = NSSelectorFromString(@"setCallbackQueue:");
            if ([result respondsToSelector:setQueueSel]) {
                dispatch_queue_attr_t attr = dispatch_queue_attr_make_with_autorelease_frequency(
                    NULL, DISPATCH_AUTORELEASE_FREQUENCY_WORK_ITEM);
                dispatch_queue_t queue = dispatch_queue_create(
                    "translation-worker.engine", attr);
                [result performSelector:setQueueSel withObject:queue];
            }
        }
        return result;
    } @catch (NSException *e) {
        fprintf(stderr, "translation-worker: createEngine: %s\n", safeUTF8(e.reason));
        return NULL;
    }
}

static NSString* extractText(id first) {
    @try {
        if ([first isKindOfClass:[NSString class]]) {
            return first;
        }
        // EMTResult — join token texts respecting spacing.
        NSArray *tokens = [first valueForKey:@"tokens"];
        if (![tokens isKindOfClass:[NSArray class]]) {
            return [first description] ?: @"";
        }
        NSMutableString *joined = [NSMutableString string];
        for (id token in tokens) {
            NSString *text = [token valueForKey:@"text"];
            if (![text isKindOfClass:[NSString class]]) continue;
            if (joined.length > 0) {
                // Check if precededBySpace on this token.
                NSNumber *preceded = [token valueForKey:@"precededBySpace"];
                if ([preceded respondsToSelector:@selector(boolValue)] && [preceded boolValue]) {
                    [joined appendString:@" "];
                }
            }
            [joined appendString:text];
        }
        return joined;
    } @catch (NSException *e) {
        fprintf(stderr, "translation-worker: extractText: %s\n", safeUTF8(e.reason));
        return @"";
    }
}

static NSString* engineTranslate(id engine, const char *srcLang, const char *tgtLang, NSString *input) {
    @try {
        SEL transSel = NSSelectorFromString(@"translateString:from:to:completion:");
        NSMethodSignature *sig = [engine methodSignatureForSelector:transSel];
        if (!sig) return NULL;

        NSLocale *srcLocale = [[NSLocale alloc] initWithLocaleIdentifier:@(srcLang)];
        NSLocale *tgtLocale = [[NSLocale alloc] initWithLocaleIdentifier:@(tgtLang)];

        __block NSString *result = nil;
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);

        void (^completion)(id, NSError*) = ^(id res, NSError *err) {
            @try {
                if ([res isKindOfClass:[NSArray class]] && [(NSArray*)res count] > 0) {
                    result = extractText([(NSArray*)res firstObject]);
                }
            } @catch (NSException *e) {
                fprintf(stderr, "translation-worker: completion: %s\n", safeUTF8(e.reason));
            }
            dispatch_semaphore_signal(sem);
        };

        NSInvocation *inv = [NSInvocation invocationWithMethodSignature:sig];
        [inv setTarget:engine];
        [inv setSelector:transSel];
        [inv setArgument:&input atIndex:2];
        [inv setArgument:&srcLocale atIndex:3];
        [inv setArgument:&tgtLocale atIndex:4];
        [inv setArgument:&completion atIndex:5];
        [inv retainArguments];
        [inv invoke];

        if (dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 30 * NSEC_PER_SEC)) != 0) {
            return NULL;
        }
        return result ?: @"";
    } @catch (NSException *e) {
        fprintf(stderr, "translation-worker: engineTranslate: %s\n", safeUTF8(e.reason));
        return NULL;
    }
}

static NSString* readPayload(FILE *stream, char **line, size_t *linecap) {
    ssize_t len = getline(line, linecap, stream);
    if (len <= 0) return nil;

    long byteLen = 0;
    if (!parseLongLine(*line, 0, 100000000, &byteLen)) return nil;

    NSMutableData *data = [NSMutableData dataWithLength:(NSUInteger)byteLen];
    if (!data) return nil;
    if (byteLen > 0) {
        size_t readLen = fread(data.mutableBytes, 1, (size_t)byteLen, stream);
        if (readLen != (size_t)byteLen) return nil;
    }

    NSString *payload = [[NSString alloc] initWithBytes:data.bytes
                                                 length:data.length
                                               encoding:NSUTF8StringEncoding];
    return payload;
}

static void writePayload(FILE *stream, NSString *payload) {
    NSData *data = [payload dataUsingEncoding:NSUTF8StringEncoding] ?: [NSData data];
    fprintf(stream, "%lu\n", (unsigned long)data.length);
    if (data.length > 0) {
        fwrite(data.bytes, 1, data.length, stream);
    }
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc != 5) {
            fprintf(stderr, "Usage: translation-worker <assets-dir> <num-engines> <src-lang> <tgt-lang>\n");
            return 1;
        }

        int protocolFd = dup(STDOUT_FILENO);
        if (protocolFd < 0) {
            fprintf(stderr, "translation-worker: dup stdout failed\n");
            return 1;
        }

        FILE *protocolOut = fdopen(protocolFd, "w");
        if (!protocolOut) {
            fprintf(stderr, "translation-worker: fdopen stdout failed\n");
            close(protocolFd);
            return 1;
        }
        setvbuf(protocolOut, NULL, _IONBF, 0);

        int stderrFd = dup(STDERR_FILENO);
        if (stderrFd >= 0) {
            dup2(stderrFd, STDOUT_FILENO);
            close(stderrFd);
        } else {
            freopen("/dev/null", "w", stdout);
        }

        const char *assetsDir = argv[1];
        int numEngines = atoi(argv[2]);
        const char *srcLang = argv[3];
        const char *tgtLang = argv[4];

        if (numEngines < 1) numEngines = 1;
        if (numEngines > 32) numEngines = 32;

        // Create engine pool.
        NSMutableArray *engines = [NSMutableArray arrayWithCapacity:numEngines];
        for (int i = 0; i < numEngines; i++) {
            id engine = createEngine(assetsDir);
            if (engine) [engines addObject:engine];
        }

        if (engines.count == 0) {
            fprintf(stderr, "translation-worker: no engines created\n");
            return 1;
        }

        fprintf(stderr, "translation-worker: %d engines ready\n", (int)engines.count);

        dispatch_queue_t concurrentQueue = dispatch_queue_create(
            "translation-worker.work", DISPATCH_QUEUE_CONCURRENT);

        char *line = NULL;
        size_t linecap = 0;

        // Main loop: read count, then that many texts.
        while (1) {
            // Read count.
            ssize_t len = getline(&line, &linecap, stdin);
            if (len <= 0) break;
            long count = 0;
            if (!parseLongLine(line, 0, 1000000, &count)) break;
            if (count == 0) break;

            // Read length-prefixed UTF-8 texts.
            NSMutableArray *inputs = [NSMutableArray arrayWithCapacity:(NSUInteger)count];
            for (long i = 0; i < count; i++) {
                NSString *input = readPayload(stdin, &line, &linecap);
                if (!input) break;
                [inputs addObject:input];
            }

            NSUInteger n = inputs.count;
            if (n != (NSUInteger)count) {
                break;
            }

            // Pre-allocate results with NSNull sentinels.
            NSMutableArray *results = [NSMutableArray arrayWithCapacity:n];
            for (NSUInteger i = 0; i < n; i++) {
                [results addObject:[NSNull null]];
            }

            dispatch_group_t group = dispatch_group_create();
            __block int nextEngine = 0;
            NSLock *lock = [[NSLock alloc] init];

            for (NSUInteger i = 0; i < n; i++) {
                NSString *text = inputs[i];
                if (text.length == 0) {
                    results[i] = @"";
                    continue;
                }

                [lock lock];
                id engine = engines[nextEngine % engines.count];
                nextEngine++;
                [lock unlock];

                dispatch_group_async(group, concurrentQueue, ^{
                    NSString *translated = engineTranslate(engine, srcLang, tgtLang, text);
                    @synchronized (results) {
                        results[i] = translated ?: @"";
                    }
                });
            }

            dispatch_group_wait(group, DISPATCH_TIME_FOREVER);

            // Output results in order.
            for (NSUInteger i = 0; i < n; i++) {
                NSString *r = results[i];
                if ((id)r == [NSNull null]) {
                    writePayload(protocolOut, @"");
                } else {
                    writePayload(protocolOut, r);
                }
            }
            fflush(protocolOut);
        }

        free(line);
        fclose(protocolOut);
        fprintf(stderr, "translation-worker: exiting\n");
    }
    return 0;
}
