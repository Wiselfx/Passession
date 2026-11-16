package sh.paseo.sessions;

import android.Manifest;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

import androidx.core.app.NotificationCompat;
import androidx.core.app.NotificationManagerCompat;
import androidx.core.app.ServiceCompat;
import androidx.core.content.ContextCompat;

import org.json.JSONException;
import org.json.JSONObject;

/**
 * Foreground service shown while Embedded Tailscale is enabled. It owns no
 * networking: the node lives in the native app, and a running foreground
 * service keeps this process out of the cached/frozen state while the activity
 * is in the background. The ongoing notification reports the state Rust sends
 * ({@code tailnetPresence}) and forwards Pause/Resume/Stop back to Rust as
 * {@code tailnetControl} events.
 *
 * <p>All static state is owned by the main thread. The service exists only
 * while Rust advertises a presence in this process, so it never outlives the
 * node it reports.
 */
public final class PaseoTailnetService extends Service {
    static final String CHANNEL = "tailnet";
    private static final String TAG = "PaseoTailnet";
    private static final String CHANNEL_FALLBACK = "Embedded Tailscale";
    private static final String EXTRA_CONTROL = "sh.paseo.sessions.tailnet.control";
    private static final int NOTIFICATION_ID = 0x7a11e7;
    private static final int REQUEST_OPEN = 0x7a11e7;
    private static final int REQUEST_PAUSE = 1, REQUEST_RESUME = 2, REQUEST_STOP = 3;

    /** The presence Rust currently advertises; null when withdrawn. */
    private static JSONObject desired;
    /** The last advertised presence, for a start that must still enter the foreground. */
    private static JSONObject last;
    /** startForegroundService was issued and onStartCommand has not run yet. */
    private static boolean starting;
    /** Android refused the start (background start restriction); retried on resume. */
    private static boolean deferred;
    /** The native app shut down: nothing is advertised again in this process. */
    private static boolean closed;
    private static String channelName;
    private static PaseoTailnetService instance;

    private boolean foreground;

    /** Starts, updates ({@code presence} non-null) or stops the service. */
    static void present(Context context, JSONObject presence) {
        if (closed) return;
        desired = presence;
        if (presence != null) last = presence;
        else deferred = false;
        if (instance != null) instance.apply();
        else if (presence != null && !starting) start(context);
    }

    /** The activity resumed: a start Android refused in the background may run now. */
    static void retry(Context context) {
        if (deferred && desired != null && instance == null && !starting) start(context);
    }

    /**
     * Re-posts the advertised notification. Android does not show a foreground
     * service notification posted while POST_NOTIFICATIONS was missing, even
     * after it is granted; posting again makes it appear.
     */
    static void refresh() {
        if (instance != null && desired != null && instance.foreground) {
            instance.post(instance.build(desired, true));
        }
    }

    /** The final native shutdown: withdraw the presence for the rest of the process. */
    static void close() {
        desired = null;
        deferred = false;
        closed = true;
        if (instance != null) instance.apply();
    }

    private static void start(Context context) {
        try {
            ContextCompat.startForegroundService(context, new Intent(context, PaseoTailnetService.class));
            starting = true;
            deferred = false;
        } catch (IllegalStateException | SecurityException error) {
            // ForegroundServiceStartNotAllowedException (API 31+) while the app is
            // in the background; the node keeps running without the service.
            deferred = true;
            Log.w(TAG, "Embedded Tailscale service start deferred until the app resumes", error);
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        instance = this;
    }

    @Override
    public void onDestroy() {
        if (instance == this) instance = null;
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        boolean requested = starting;
        starting = false;
        String control = intent == null ? null : intent.getStringExtra(EXTRA_CONTROL);
        if (control != null && foreground) {
            control(control);
            return START_NOT_STICKY;
        }
        // startForegroundService obliges startForeground even when Rust withdrew
        // the presence before the service started; apply() then stops it.
        if (requested && desired == null && last != null) enterForeground(build(last, false));
        // A control that started a service with nothing to advertise stops it here.
        apply();
        // Never restarted without the native app that advertises the presence.
        return START_NOT_STICKY;
    }

    private void apply() {
        JSONObject presence = desired;
        if (presence == null) {
            stop();
            return;
        }
        Notification notification = build(presence, true);
        if (foreground) post(notification);
        else enterForeground(notification);
    }

    private void enterForeground(Notification notification) {
        try {
            ServiceCompat.startForeground(this, NOTIFICATION_ID, notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
            foreground = true;
        } catch (RuntimeException error) {
            // Android 12+ may refuse the foreground start after the app left the
            // foreground; retried on the next resume.
            deferred = desired != null;
            Log.w(TAG, "Embedded Tailscale service could not enter the foreground", error);
            stop();
        }
    }

    private void stop() {
        // A later present() starts a new service rather than reusing this stopping one.
        if (instance == this) instance = null;
        if (foreground) {
            ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE);
            foreground = false;
        }
        stopSelf();
    }

    /** A notification action. Ignored unless the native app currently advertises a presence. */
    private void control(String action) {
        JSONObject presence = desired;
        if (presence == null || instance != this) return;
        try {
            PaseoBridge.nativeEvent("tailnetControl", new JSONObject().put("action", action).toString());
        } catch (JSONException error) {
            return;
        }
        // Stop is final: drop the actions until Rust withdraws the presence.
        if ("stop".equals(action) && foreground) post(build(presence, false));
    }

    /** Without POST_NOTIFICATIONS (API 33+) the service still runs; Android hides the notification. */
    private void post(Notification notification) {
        if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            return;
        }
        NotificationManagerCompat.from(this).notify(NOTIFICATION_ID, notification);
    }

    private Notification build(JSONObject presence, boolean actions) {
        String title = presence.optString("title");
        ensureChannel(title.isEmpty() ? CHANNEL_FALLBACK : title);
        // No payload to navigate to: return to the app as the launcher does.
        PendingIntent open = PendingIntent.getActivity(this, REQUEST_OPEN,
                PaseoNotifications.resumeApp(this),
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        NotificationCompat.Builder builder = new NotificationCompat.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.paseo_notification_icon)
                .setContentTitle(title)
                .setContentText(presence.optString("status"))
                .setContentIntent(open)
                .setOngoing(true)
                .setSilent(true)
                .setOnlyAlertOnce(true)
                .setShowWhen(false)
                .setCategory(NotificationCompat.CATEGORY_SERVICE)
                .setPriority(NotificationCompat.PRIORITY_LOW)
                .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE);
        if (actions) {
            if ("paused".equals(presence.optString("state"))) {
                builder.addAction(0, presence.optString("resume"), controlIntent("resume", REQUEST_RESUME));
            } else {
                builder.addAction(0, presence.optString("pause"), controlIntent("pause", REQUEST_PAUSE));
            }
            builder.addAction(0, presence.optString("stop"), controlIntent("stop", REQUEST_STOP));
        }
        return builder.build();
    }

    private PendingIntent controlIntent(String action, int requestCode) {
        Intent intent = new Intent(this, PaseoTailnetService.class).putExtra(EXTRA_CONTROL, action);
        if (Build.VERSION.SDK_INT >= 29) intent.setIdentifier("tailnet:" + action);
        return PendingIntent.getService(this, requestCode, intent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
    }

    /** Low importance and silent; re-creating an existing channel only renames it. */
    private void ensureChannel(String name) {
        if (Build.VERSION.SDK_INT < 26 || name.equals(channelName)) return;
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (manager == null) return;
        NotificationChannel channel = new NotificationChannel(CHANNEL, name, NotificationManager.IMPORTANCE_LOW);
        channel.setShowBadge(false);
        manager.createNotificationChannel(channel);
        channelName = name;
    }
}
