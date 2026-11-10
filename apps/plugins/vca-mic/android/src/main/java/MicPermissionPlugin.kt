package io.metavu.voicecommandai.mic

import android.Manifest
import android.app.Activity
import app.tauri.annotation.Permission
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Plugin

/**
 * Declares the `microphone` permission alias. Tauri's built-in
 * `checkPermissions` and `requestPermissions` commands (inherited from
 * [Plugin]) do the rest; the Rust side calls them before cpal opens the
 * input stream.
 */
@TauriPlugin(
    permissions = [
        Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "microphone"),
    ],
)
class MicPermissionPlugin(activity: Activity) : Plugin(activity)
