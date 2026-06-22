#import <Foundation/Foundation.h>
#import <objc/runtime.h>
#import <objc/message.h>
#import <dlfcn.h>

static bool frameworksLoaded = false;

id createEngine(const char *assetsDir) {
    if (!frameworksLoaded) {
        dlopen("/System/Library/PrivateFrameworks/EmbeddedAcousticRecognition.framework/EmbeddedAcousticRecognition", RTLD_LAZY);
        dlopen("/System/Library/PrivateFrameworks/TranslationDaemon.framework/TranslationDaemon", RTLD_LAZY);
        frameworksLoaded = true;
    }

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
                    "com.apple-translate-rs-sync.engine", attr);
                [result performSelector:setQueueSel withObject:queue];
            }
        }

        return result;
    } @catch (NSException *e) {
        fprintf(stderr, "apple-translate-rs-sync: createEngine exception: %s\n",
                [e.reason UTF8String]);
        return NULL;
    }
}

NSString* engineTranslate(id engine, const char *srcLang, const char *tgtLang, const char *text) {
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
                result = [(NSArray*)res firstObject];
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
        return result;
    } @catch (NSException *e) {
        fprintf(stderr, "apple-translate-rs-sync: engineTranslate ObjC exception: %s\n",
                [e.reason UTF8String]);
        return NULL;
    }
}
