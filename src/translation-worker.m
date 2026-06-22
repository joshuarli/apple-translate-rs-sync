// translation-worker: standalone ObjC binary hosting EMTTranslator engines.
// Uses a count-based batch protocol over stdin/stdout so the worker stays
// alive across batches (no per-batch process startup).
//
// Protocol (stdin):
//   <count>\n        — number of texts in this batch (0 = exit)
//   <text>\n         — one source text per line, repeated <count> times
//
// Protocol (stdout):
//   <translated>\n   — one translated text per line, same order as inputs
//
// Usage: translation-worker <assets-dir> <num-engines> <src-lang> <tgt-lang>

#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#import <objc/message.h>
#import <dlfcn.h>

static id createEngine(const char *assetsDir) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        dlopen("/System/Library/PrivateFrameworks/EmbeddedAcousticRecognition.framework/EmbeddedAcousticRecognition", RTLD_LAZY);
        dlopen("/System/Library/PrivateFrameworks/TranslationDaemon.framework/TranslationDaemon", RTLD_LAZY);
    });

    Class EMTCls = NSClassFromString(@"EMTTranslator");
    if (!EMTCls) return NULL;

    NSURL *url = [NSURL fileURLWithPath:@(assetsDir)];
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
        fprintf(stderr, "translation-worker: createEngine: %s\n", [e.reason UTF8String]);
        return NULL;
    }
}

static NSString* extractText(id first) {
    if ([first isKindOfClass:[NSString class]]) {
        return first;
    }
    // EMTResult — join token texts respecting spacing.
    NSArray *tokens = [first valueForKey:@"tokens"];
    if (![tokens isKindOfClass:[NSArray class]]) {
        return [first description];
    }
    NSMutableString *joined = [NSMutableString string];
    for (id token in tokens) {
        NSString *text = [token valueForKey:@"text"];
        if (!text) continue;
        if (joined.length > 0) {
            // Check if precededBySpace on this token.
            NSNumber *preceded = [token valueForKey:@"precededBySpace"];
            if (preceded && [preceded boolValue]) {
                [joined appendString:@" "];
            }
        }
        [joined appendString:text];
    }
    return joined;
}

static NSString* engineTranslate(id engine, const char *srcLang, const char *tgtLang, const char *text) {
    @try {
        SEL transSel = NSSelectorFromString(@"translateString:from:to:completion:");
        NSMethodSignature *sig = [engine methodSignatureForSelector:transSel];
        if (!sig) return NULL;

        NSLocale *srcLocale = [[NSLocale alloc] initWithLocaleIdentifier:@(srcLang)];
        NSLocale *tgtLocale = [[NSLocale alloc] initWithLocaleIdentifier:@(tgtLang)];
        NSString *input = @(text);

        __block NSString *result = nil;
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);

        void (^completion)(id, NSError*) = ^(id res, NSError *err) {
            if ([res isKindOfClass:[NSArray class]] && [(NSArray*)res count] > 0) {
                result = extractText([(NSArray*)res firstObject]);
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

        dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 30 * NSEC_PER_SEC));
        return result ?: @"";
    } @catch (NSException *e) {
        fprintf(stderr, "translation-worker: engineTranslate: %s\n", [e.reason UTF8String]);
        return NULL;
    }
}

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc != 5) {
            fprintf(stderr, "Usage: translation-worker <assets-dir> <num-engines> <src-lang> <tgt-lang>\n");
            return 1;
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
            long count = strtol(line, NULL, 10);
            if (count == 0) break;
            if (count < 0 || count > 1000000) break;

            // Read texts.
            NSMutableArray *inputs = [NSMutableArray arrayWithCapacity:(NSUInteger)count];
            for (long i = 0; i < count; i++) {
                len = getline(&line, &linecap, stdin);
                if (len <= 0) break;
                // Trim trailing newline.
                if (len > 0 && line[len - 1] == '\n') line[len - 1] = '\0';
                [inputs addObject:@(line)];
            }

            NSUInteger n = inputs.count;
            if (n == 0) {
                printf("0\n");
                fflush(stdout);
                continue;
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
                    const char *cText = [text UTF8String];
                    NSString *translated = engineTranslate(engine, srcLang, tgtLang, cText);
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
                    printf("\n");
                } else {
                    printf("%s\n", [r UTF8String]);
                }
            }
            fflush(stdout);
        }

        free(line);
        fprintf(stderr, "translation-worker: exiting\n");
    }
    return 0;
}
