package io.metavu.voicecommandai.share

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.ClipData
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Handler
import android.os.Looper
import androidx.activity.result.ActivityResult
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File

/**
 * This library's own FileProvider, so it does not clash with a provider the
 * app declares. It exposes only `cache/vca-share/` (res/xml/vca_share_paths.xml).
 */
class ShareFileProvider : FileProvider()

/**
 * The system share sheet for a text file. Android System WebView has no Web
 * Share API, so vca-shell's `vca_share_text_file` calls [shareFile] from Rust.
 *
 * The text is written to `cache/vca-share/<name>` (the previous shared file
 * is removed) and sent with ACTION_SEND as a read-only content URI. The
 * chooser reports the picked target through a PendingIntent; when the
 * chooser closes, the call resolves `{ shared }`.
 */
@TauriPlugin
class ShareFilePlugin(private val activity: Activity) : Plugin(activity) {
    private val main = Handler(Looper.getMainLooper())

    /** Set when the chooser reports that the user picked a target. */
    @Volatile private var chosen = false
    private var receiver: BroadcastReceiver? = null

    @Command
    fun shareFile(invoke: Invoke) {
        val args = invoke.getArgs()
        val name = args.getString("name", null)
        val text = args.getString("text", null)
        val mime = args.getString("mime", null) ?: "text/plain"
        if (name.isNullOrEmpty() || text == null) {
            invoke.reject("missing file name or text", "share_failed")
            return
        }
        val file = try {
            write(name, text)
        } catch (e: Exception) {
            invoke.reject("could not write the file: ${e.message}", "share_failed")
            return
        }
        main.post { open(invoke, file, name, mime) }
    }

    /** Writes the only file in `cache/vca-share/`; `name` must stay inside it. */
    private fun write(name: String, text: String): File {
        val dir = File(activity.cacheDir, DIR)
        dir.deleteRecursively()
        if (!dir.mkdirs() && !dir.isDirectory) throw IllegalStateException("cannot create $dir")
        val file = File(dir, name)
        if (file.canonicalFile.parentFile != dir.canonicalFile) {
            throw IllegalArgumentException("not a plain file name")
        }
        file.writeText(text, Charsets.UTF_8)
        return file
    }

    private fun open(invoke: Invoke, file: File, name: String, mime: String) {
        val uri = try {
            FileProvider.getUriForFile(activity, "${activity.packageName}$AUTHORITY_SUFFIX", file)
        } catch (e: IllegalArgumentException) {
            invoke.reject("could not share the file: ${e.message}", "share_failed")
            return
        }
        val send = Intent(Intent.ACTION_SEND).apply {
            type = mime
            putExtra(Intent.EXTRA_STREAM, uri)
            putExtra(Intent.EXTRA_TITLE, name)
            // The grant travels with the clip data through the chooser.
            clipData = ClipData.newRawUri(name, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        listenForChoice()
        val picked = PendingIntent.getBroadcast(
            activity,
            0,
            Intent(chosenAction()).setPackage(activity.packageName),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val chooser = Intent.createChooser(send, null, picked.intentSender)
        try {
            startActivityForResult(invoke, chooser, "chooserClosed")
        } catch (e: Exception) {
            stopListening()
            invoke.reject("could not open the share sheet: ${e.message}", "share_failed")
        }
    }

    @ActivityCallback
    private fun chooserClosed(invoke: Invoke, @Suppress("UNUSED_PARAMETER") result: ActivityResult) {
        // The chooser closes as it starts the picked target; the broadcast
        // naming that target can arrive just after this result.
        main.postDelayed({
            val shared = chosen
            stopListening()
            invoke.resolve(JSObject().put("shared", shared))
        }, CHOICE_GRACE_MS)
    }

    private fun chosenAction() = "${activity.packageName}.VCA_SHARE_CHOSEN"

    private fun listenForChoice() {
        stopListening()
        chosen = false
        val listener = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                chosen = true
            }
        }
        // Not exported: only this app's own PendingIntent reaches it.
        ContextCompat.registerReceiver(
            activity,
            listener,
            IntentFilter(chosenAction()),
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        receiver = listener
    }

    private fun stopListening() {
        receiver?.let {
            try {
                activity.unregisterReceiver(it)
            } catch (_: IllegalArgumentException) {
                // Already unregistered.
            }
        }
        receiver = null
    }

    private companion object {
        const val DIR = "vca-share"
        const val AUTHORITY_SUFFIX = ".vcashare"
        const val CHOICE_GRACE_MS = 400L
    }
}
