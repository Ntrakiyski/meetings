#import <AppKit/AppKit.h>
#import <AuthenticationServices/AuthenticationServices.h>

typedef void (*MeetinglyWebAuthCompletion)(const char *callbackURL, const char *errorMessage);

@interface MeetinglyPresentationContext : NSObject <ASWebAuthenticationPresentationContextProviding>
@property(nonatomic, strong) NSWindow *window;
@end

@implementation MeetinglyPresentationContext
- (ASPresentationAnchor)presentationAnchorForWebAuthenticationSession:(ASWebAuthenticationSession *)session {
    (void)session;
    return self.window ?: NSApplication.sharedApplication.keyWindow ?: NSApplication.sharedApplication.windows.firstObject;
}
@end

static ASWebAuthenticationSession *meetinglySession;
static MeetinglyPresentationContext *meetinglyPresentationContext;
static id meetinglyResignObserver;

static void meetinglyStopObservingApplicationFocus(void) {
    if (meetinglyResignObserver != nil) {
        [NSNotificationCenter.defaultCenter removeObserver:meetinglyResignObserver];
        meetinglyResignObserver = nil;
    }
}

static void meetinglyForegroundAuthenticationBrowser(NSURL *authorizationURL) {
    NSURL *browserURL = [NSWorkspace.sharedWorkspace URLForApplicationToOpenURL:authorizationURL];
    NSString *browserIdentifier = [NSBundle bundleWithURL:browserURL].bundleIdentifier;
    if (browserIdentifier.length == 0) {
        return;
    }

    for (NSRunningApplication *application in
         [NSRunningApplication runningApplicationsWithBundleIdentifier:browserIdentifier]) {
        if (!application.terminated) {
            [application activateWithOptions:NSApplicationActivateAllWindows];
            return;
        }
    }
}

static void meetinglyObserveAuthenticationBrowserLaunch(NSURL *authorizationURL) {
    meetinglyStopObservingApplicationFocus();
    meetinglyResignObserver = [NSNotificationCenter.defaultCenter
        addObserverForName:NSApplicationDidResignActiveNotification
                    object:NSApplication.sharedApplication
                     queue:NSOperationQueue.mainQueue
                usingBlock:^(__unused NSNotification *notification) {
                    meetinglyStopObservingApplicationFocus();
                    dispatch_after(
                        dispatch_time(DISPATCH_TIME_NOW, (int64_t)(250 * NSEC_PER_MSEC)),
                        dispatch_get_main_queue(),
                        ^{
                            meetinglyForegroundAuthenticationBrowser(authorizationURL);
                        });
                }];
}

int meetingly_start_web_auth_session(
    const char *urlString,
    const char *callbackScheme,
    void *presentationWindow,
    MeetinglyWebAuthCompletion completion
) {
    if (urlString == NULL || callbackScheme == NULL || presentationWindow == NULL || completion == NULL) {
        return 0;
    }

    NSString *authorizationString = [NSString stringWithUTF8String:urlString];
    NSString *scheme = [NSString stringWithUTF8String:callbackScheme];
    NSURL *authorizationURL = [NSURL URLWithString:authorizationString];
    if (authorizationURL == nil || scheme == nil) {
        return 0;
    }

    dispatch_async(dispatch_get_main_queue(), ^{
        if (@available(macOS 10.15, *)) {
            meetinglyPresentationContext = [MeetinglyPresentationContext new];
            meetinglyPresentationContext.window = (__bridge NSWindow *)presentationWindow;

            meetinglySession = [[ASWebAuthenticationSession alloc]
                initWithURL:authorizationURL
                callbackURLScheme:scheme
                completionHandler:^(NSURL *callbackURL, NSError *error) {
                    meetinglyStopObservingApplicationFocus();
                    NSString *callback = callbackURL.absoluteString;
                    NSString *message = error.localizedDescription;
                    completion(callback.UTF8String, message.UTF8String);
                    meetinglySession = nil;
                    meetinglyPresentationContext = nil;
                }];
            meetinglySession.presentationContextProvider = meetinglyPresentationContext;
            meetinglySession.prefersEphemeralWebBrowserSession = NO;
            meetinglyObserveAuthenticationBrowserLaunch(authorizationURL);

            if (![meetinglySession start]) {
                meetinglyStopObservingApplicationFocus();
                completion(NULL, "macOS could not start the web authentication session.");
                meetinglySession = nil;
                meetinglyPresentationContext = nil;
            }
        } else {
            completion(NULL, "Meetily authentication requires macOS 10.15 or newer.");
        }
    });

    return 1;
}
