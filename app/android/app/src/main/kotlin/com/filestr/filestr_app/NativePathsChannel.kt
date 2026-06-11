package com.filestr.filestr_app

import android.content.Context
import android.content.Intent
import android.net.Uri
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel

/**
 * Exposes the few host paths the daemon needs (its executable's location and
 * the app's private storage dirs). Registered on BOTH the UI engine and the
 * foreground-service engine, since the service isolate is what actually spawns
 * the daemon and flutter_foreground_task does not run the plugin registrant on
 * the service engine.
 */
object NativePathsChannel {
    private const val CHANNEL = "filestr/native"

    fun register(messenger: BinaryMessenger, context: Context) {
        val appContext = context.applicationContext
        MethodChannel(messenger, CHANNEL).setMethodCallHandler { call, result ->
            when (call.method) {
                "getPaths" -> result.success(
                    mapOf(
                        // Where the bundled libfilestrd.so daemon is unpacked, executable.
                        "nativeLibDir" to appContext.applicationInfo.nativeLibraryDir,
                        "filesDir" to appContext.filesDir.absolutePath,
                        "cacheDir" to appContext.cacheDir.absolutePath,
                    )
                )
                // Open Grayjay's Add Source flow on our plugin config URL. Grayjay's
                // own "Install by URL" dialog rejects http://localhost, but its
                // exported AddSourceActivity accepts a VIEW intent with the URL.
                "openInGrayjay" -> {
                    val url = call.argument<String>("url")
                    try {
                        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url)).apply {
                            setClassName(
                                "com.futo.platformplayer",
                                "com.futo.platformplayer.activities.AddSourceActivity"
                            )
                            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                        }
                        appContext.startActivity(intent)
                        result.success(true)
                    } catch (e: Exception) {
                        result.success(false)
                    }
                }
                else -> result.notImplemented()
            }
        }
    }
}
