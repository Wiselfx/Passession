package sh.paseo.sessions;

import android.app.Activity;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;

/**
 * Trampoline for {@code paseo://} links. A link opened from another app starts
 * this activity inside that app's task; forwarding with NEW_TASK moves the
 * launch into the package task, so a second MainActivity is never created in
 * someone else's task. The forward is the launcher's own intent with the link in
 * {@link PaseoBridge#EXTRA_LINK} (see {@link PaseoNotifications#openApp}). No UI:
 * it finishes in onCreate.
 */
public class PaseoLinkActivity extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        Intent source = getIntent();
        Uri link = source.getData();
        if (Intent.ACTION_VIEW.equals(source.getAction()) && link != null && "paseo".equals(link.getScheme())) {
            startActivity(PaseoNotifications.openApp(this).putExtra(PaseoBridge.EXTRA_LINK, link.toString()));
        }
        finish();
    }
}
