package io.metavu.voicecommandai.push

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.webkit.WebView
import app.tauri.annotation.Command
import app.tauri.annotation.Permission
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import com.google.firebase.messaging.FirebaseMessaging

/**
 * The FCM device token and the "opened from a notification" signal. Firebase
 * shows notification messages itself while the app is closed. The token goes
 * to Rust only and is never logged. DNSPilot's spec does not say where a push
 * carries its ciphertext, so a tap reports "opened" without a payload.
 */
@TauriPlugin(
    permissions = [
        Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "notifications")
    ]
)
class PushPlugin(private val activity: Activity) : Plugin(activity) {
    private var opened = false

    override fun load(webView: WebView) {
        super.load(webView)
        note(activity.intent)
    }

    override fun onNewIntent(intent: Intent) {
        note(intent)
    }

    /** Firebase puts this extra on the intent of a tapped notification. */
    private fun note(intent: Intent?) {
        if (intent?.hasExtra(MESSAGE_ID) == true) {
            opened = true
            intent.removeExtra(MESSAGE_ID)
        }
    }

    @Command
    fun token(invoke: Invoke) {
        val messaging = try {
            FirebaseMessaging.getInstance()
        } catch (_: IllegalStateException) {
            // No google-services.json in this build.
            invoke.reject("push_not_configured")
            return
        }
        messaging.token.addOnCompleteListener { task ->
            if (task.isSuccessful && task.result != null) {
                invoke.resolve(JSObject().put("token", task.result))
            } else {
                invoke.reject("push_registration_failed")
            }
        }
    }

    @Command
    fun takeOpened(invoke: Invoke) {
        val answer = JSObject().put("opened", opened)
        opened = false
        invoke.resolve(answer)
    }

    private companion object {
        const val MESSAGE_ID = "google.message_id"
    }
}
