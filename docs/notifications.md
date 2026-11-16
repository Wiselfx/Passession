# Notifications

Notifications belong to `packages/gui-platform`: `src/mobile` owns push registration and token lifecycle; `src/android` plus `android/java/sh/paseo/sessions` owns Android delivery; `src/ios` owns APNs/native iOS callbacks; `src/desktop` owns desktop delivery and activation. `notification_policy.rs` owns shared policy. Platform events carry explicit targets to the Rust shell; platform code does not select another pane's global route.

## Identity and delivery

A notification target identifies the host and session. Resolve it against saved hosts and current daemon data before opening; do not select an equal session/path on another host or create a replacement session when the target is unavailable. Wait for application initialization and host readiness, deduplicate delivery, and expose stale/unreachable-target failures.

The native mobile implementation uses Firebase messaging on Android and APNs directly on iOS (the iOS app does not link Firebase), plus native local notifications. Both register their device token through the existing push service's Expo-compatible token exchange, and Android keeps Expo-formatted FCM application data. That service compatibility is not an Expo/Metro application runtime or a web client.

OS-rendered background notifications and locally presented foreground notifications have different ownership. Avoid duplicate alerts. Background handler errors must remain available to the foreground UI rather than being silently discarded. Notification permission is a user choice, not a daemon requirement.

## Permission

The app never asks at launch. A GUI flow explains why it needs notifications at the moment a feature needs them, then calls `PlatformHandle::request_notification_permission`, which shows the OS prompt when the OS still offers one. On Android and iOS this asks for the local notification permission whether or not the build is provisioned for push; when push is provisioned and the permission is granted, the same request also registers the push token. Launch and resume only re-sync the token silently.

`PermissionState` reports what asking would do:

- `NotDetermined`: asking shows the OS prompt.
- `Denied`: refused, but asking may still prompt (Android while it would show a rationale).
- `Blocked`: the OS will not prompt again. Offer `PlatformHandle::open_permission_settings` instead.
- `Granted` or `Provisional`: allowed.

Per platform:

- Android 13+ records that the prompt was requested and uses `shouldShowRequestPermissionRationale` to tell `Denied` from `Blocked`. Below Android 13, notifications turned off in system settings are `Blocked`.
- iOS and macOS ask once, so a refusal is `Blocked`.
- Windows reads the app's toast setting; anything but enabled is `Blocked`.
- Linux reports `Blocked` when no notification server answers `GetServerInformation` within 2 seconds.

`open_permission_settings` opens the app's notification settings: one channel on Android (for example `tailnet`), the notification settings URL on iOS 16+ and the app's Settings page before that, the app's page in System Settings › Notifications on macOS, and `ms-settings:notifications` on Windows. Linux has no such screen and fails `unsupported`.

Recheck the OS permission when the app returns to the foreground (`Event::Resume`, `Event::Foreground`, `Event::AppGotFocus`); a cached result may predate a change in system settings. Reading that status never prompts.

### In-app flows

- After a host connects (main window), the shell reads the status once per session. If asking would prompt (`NotDetermined`, or `Denied` on Android), it shows a one-time explanation with "Allow notifications". The answer, allow or dismiss, sets the `notificationPermissionAsked` preference, so the explanation never shows again on that install.
- Turning on Embedded Tailscale on Android shows its own explanation under the same preference: the node's status and its Pause/Stop controls live in a notification.
- Settings › Notifications is shown on every OS:
  - Allow (`NotDetermined`/`Denied`), or Open settings (`Blocked`).
  - On Linux, `Blocked` shows "Unavailable" with a hint about the notification service.
  - The page rechecks on resume, foreground and focus.
- While the Android Tailscale page shows the node on and notifications not allowed, it shows a row with Allow or Open settings (channel `tailnet`). It shows a battery row from `background_restrictions`.
- When an attention notification fails and the status shows it is not allowed, the shell shows one toast per session with Allow or Open settings. On Linux, when no notification server is running, the toast has no action.

## Configuration and privacy

Android/iOS credentials are optional and must match the Sessions application identity. Source builds do not include production push credentials or store provisioning. Do not claim push delivery works solely because a client compiles or a token is stored. Tokens and notification payloads may expose private host/session data; keep diagnostics scoped and redact credentials.

## Verification

Exercise permission denial/grant, foreground delivery, background delivery, terminated-app launch, duplicate taps, reconnect, stale targets and Back on the actual OS/device. Linux native client tests do not establish Android/iOS notification behavior. See [mobile testing](mobile-testing.md), [Android](android.md), and [testing](testing.md).
