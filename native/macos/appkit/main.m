#import <Cocoa/Cocoa.h>
#import <WebKit/WebKit.h>
#import <notify.h>
#import <objc/message.h>
#import <objc/runtime.h>
#include <stdlib.h>
#include <string.h>

static const char *NotificationPrefix = "dev.nwipc.webkit-e2e.bundle-loaded.";
static const char *EchoNotificationPrefix = "dev.nwipc.webkit-e2e.binary-echo.";

@interface NavigationObserver : NSObject <WKNavigationDelegate>
@property(nonatomic) NSUInteger finishes;
@property(nonatomic) BOOL terminated;
@property(nonatomic, strong) NSError *failure;
@end

@implementation NavigationObserver
- (void)webView:(WKWebView *)webView didFinishNavigation:(WKNavigation *)navigation {
    (void)webView;
    (void)navigation;
    self.finishes += 1;
}
- (void)webView:(WKWebView *)webView didFailNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    (void)webView;
    (void)navigation;
    self.failure = error;
}
- (void)webViewWebContentProcessDidTerminate:(WKWebView *)webView {
    (void)webView;
    self.terminated = YES;
}
@end

static BOOL HasMethod(Class classObject, const char *name) {
    return classObject != Nil && class_getInstanceMethod(classObject, sel_registerName(name)) != NULL;
}

static id SendId(id target, const char *selectorName) {
    return ((id (*)(id, SEL))objc_msgSend)(target, sel_registerName(selectorName));
}

static id SendIdId(id target, const char *selectorName, id argument) {
    return ((id (*)(id, SEL, id))objc_msgSend)(target, sel_registerName(selectorName), argument);
}

static void SendVoidId(id target, const char *selectorName, id argument) {
    ((void (*)(id, SEL, id))objc_msgSend)(target, sel_registerName(selectorName), argument);
}

static void SendVoidIdId(id target, const char *selectorName, id first, id second) {
    ((void (*)(id, SEL, id, id))objc_msgSend)(target, sel_registerName(selectorName), first, second);
}

static void SendVoid(id target, const char *selectorName) {
    ((void (*)(id, SEL))objc_msgSend)(target, sel_registerName(selectorName));
}

static BOOL SetBundleParameter(id processPool, NSString *key, const char *environment) {
    const char *value = getenv(environment);
    if (value == NULL) {
        return NO;
    }
    SendVoidIdId(processPool, "_setObject:forBundleParameter:",
                 [NSString stringWithUTF8String:value], key);
    return YES;
}

static pid_t WebProcessIdentifier(WKWebView *webView) {
    return ((pid_t (*)(id, SEL))objc_msgSend)(webView, sel_registerName("_webProcessIdentifier"));
}

static BOOL WaitForInitialLoad(NSUInteger expectedFinishes, int notificationToken, NSTimeInterval timeout, NavigationObserver *observer) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:timeout];
    BOOL marker = NO;
    while ([deadline timeIntervalSinceNow] > 0) {
        int changed = 0;
        if (notify_check(notificationToken, &changed) == NOTIFY_STATUS_OK && changed != 0) {
            marker = YES;
        }
        if (marker && observer.finishes >= expectedFinishes) {
            return YES;
        }
        if (observer.failure != nil) {
            return NO;
        }
        [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                 beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    }
    return NO;
}

static BOOL WaitForReplacement(NSUInteger expectedFinishes, pid_t previousPID, WKWebView *webView, NSTimeInterval timeout, NavigationObserver *observer) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:timeout];
    while ([deadline timeIntervalSinceNow] > 0) {
        pid_t currentPID = WebProcessIdentifier(webView);
        if (observer.finishes >= expectedFinishes && currentPID > 0 && currentPID != previousPID) {
            return YES;
        }
        if (observer.failure != nil) {
            return NO;
        }
        [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                 beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    }
    return NO;
}

static BOOL WaitForNotification(int notificationToken, NSTimeInterval timeout) {
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:timeout];
    while ([deadline timeIntervalSinceNow] > 0) {
        int changed = 0;
        if (notify_check(notificationToken, &changed) == NOTIFY_STATUS_OK && changed != 0) {
            return YES;
        }
        [[NSRunLoop currentRunLoop] runMode:NSDefaultRunLoopMode
                                 beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    }
    return NO;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc != 3) {
            fprintf(stderr, "usage: nwipc-webkit-e2e <bundle> <timeout-seconds>\n");
            return 64;
        }

        NSString *bundlePath = [NSString stringWithUTF8String:argv[1]];
        NSTimeInterval timeout = MAX(1.0, [[NSString stringWithUTF8String:argv[2]] doubleValue]);
        Class privateConfigurationClass = NSClassFromString(@"_WKProcessPoolConfiguration");
        Class processPoolClass = NSClassFromString(@"WKProcessPool");
        if (!HasMethod(privateConfigurationClass, "setInjectedBundleURL:") ||
            !HasMethod(processPoolClass, "_initWithConfiguration:") ||
            !HasMethod(processPoolClass, "_setObject:forBundleParameter:") ||
            !HasMethod(WKWebViewConfiguration.class, "setProcessPool:") ||
            !HasMethod(WKWebView.class, "_killWebContentProcessAndResetState") ||
            !HasMethod(WKWebView.class, "_webProcessIdentifier")) {
            fprintf(stderr, "unsupported: required WebKit SPI is unavailable\n");
            return 2;
        }

        const char *loadNotification = getenv("NWIPC_WEBKIT_E2E_NOTIFICATION");
        if (loadNotification == NULL || strncmp(loadNotification, NotificationPrefix, strlen(NotificationPrefix)) != 0) {
            fprintf(stderr, "failed: invalid bundle load marker name\n");
            return 64;
        }
        int notificationToken = 0;
        if (notify_register_check(loadNotification, &notificationToken) != NOTIFY_STATUS_OK) {
            fprintf(stderr, "failed: could not register bundle load marker\n");
            return 70;
        }
        int initialNotificationState = 0;
        notify_check(notificationToken, &initialNotificationState);
        const char *echoNotification = getenv("NWIPC_WEBKIT_E2E_ECHO_NOTIFICATION");
        if (echoNotification == NULL || strncmp(echoNotification, EchoNotificationPrefix, strlen(EchoNotificationPrefix)) != 0) {
            fprintf(stderr, "failed: invalid binary echo marker name\n");
            notify_cancel(notificationToken);
            return 64;
        }
        int echoNotificationToken = 0;
        if (notify_register_check(echoNotification, &echoNotificationToken) != NOTIFY_STATUS_OK) {
            fprintf(stderr, "failed: could not register binary echo marker\n");
            notify_cancel(notificationToken);
            return 70;
        }
        notify_check(echoNotificationToken, &initialNotificationState);

        id privateConfiguration = SendId(privateConfigurationClass, "new");
        SendVoidId(privateConfiguration, "setInjectedBundleURL:", [NSURL fileURLWithPath:bundlePath]);
        id processPool = SendIdId(SendId(processPoolClass, "alloc"), "_initWithConfiguration:", privateConfiguration);
        SendVoidIdId(processPool, "_setObject:forBundleParameter:", @"1", @"nwipc.e2e.enabled");
        if (!SetBundleParameter(processPool, @"nwipc.e2e.iosurface", "NWIPC_WEBKIT_E2E_IOSURFACE") ||
            !SetBundleParameter(processPool, @"nwipc.e2e.load-notification", "NWIPC_WEBKIT_E2E_NOTIFICATION") ||
            !SetBundleParameter(processPool, @"nwipc.e2e.echo-notification", "NWIPC_WEBKIT_E2E_ECHO_NOTIFICATION") ||
            !SetBundleParameter(processPool, @"nwipc.e2e.timeout", "NWIPC_E2E_TIMEOUT_SECONDS")) {
            fprintf(stderr, "failed: missing E2E bundle parameter\n");
            notify_cancel(notificationToken);
            notify_cancel(echoNotificationToken);
            return 64;
        }

        WKWebViewConfiguration *configuration = [[WKWebViewConfiguration alloc] init];
        SendVoidId(configuration, "setProcessPool:", processPool);
        NavigationObserver *observer = [[NavigationObserver alloc] init];
        WKWebView *webView = [[WKWebView alloc] initWithFrame:NSMakeRect(0, 0, 640, 480)
                                               configuration:configuration];
        webView.navigationDelegate = observer;
        [NSApplication sharedApplication];
        NSWindow *window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 640, 480)
                                                       styleMask:NSWindowStyleMaskTitled
                                                         backing:NSBackingStoreBuffered
                                                           defer:NO];
        window.contentView = webView;
        [window orderFrontRegardless];

        [webView loadHTMLString:@"<!doctype html><title>nwipc-e2e-1</title>" baseURL:nil];
        if (!WaitForInitialLoad(1, notificationToken, timeout, observer)) {
            fprintf(stderr, "timeout: initial WebContent bundle load; navigation=%lu error=%s\n",
                    (unsigned long)observer.finishes,
                    observer.failure.localizedDescription.UTF8String ?: "none");
            notify_cancel(notificationToken);
            notify_cancel(echoNotificationToken);
            return 3;
        }
        if (!WaitForNotification(echoNotificationToken, timeout)) {
            fprintf(stderr, "timeout: renderer to native-peer binary echo\n");
            notify_cancel(notificationToken);
            notify_cancel(echoNotificationToken);
            return 4;
        }
        pid_t initialPID = WebProcessIdentifier(webView);

        SendVoid(webView, "_killWebContentProcessAndResetState");
        [webView loadHTMLString:@"<!doctype html><title>nwipc-e2e-2</title>" baseURL:nil];
        if (!WaitForReplacement(2, initialPID, webView, timeout, observer)) {
            fprintf(stderr, "timeout: replacement WebContent process; navigation=%lu terminated=%s initial-pid=%d current-pid=%d error=%s\n",
                    (unsigned long)observer.finishes,
                    observer.terminated ? "yes" : "no",
                    initialPID,
                    WebProcessIdentifier(webView),
                    observer.failure.localizedDescription.UTF8String ?: "none");
            notify_cancel(notificationToken);
            notify_cancel(echoNotificationToken);
            return 5;
        }

        notify_cancel(notificationToken);
        notify_cancel(echoNotificationToken);
        printf("webkit-e2e: initial-load=ok binary-echo=ok replacement-process=ok hardened-process=ok\n");
        return 0;
    }
}
