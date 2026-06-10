package com.filestr.filestr_app

import android.content.Context
import com.pravera.flutter_foreground_task.FlutterForegroundTaskLifecycleListener
import com.pravera.flutter_foreground_task.FlutterForegroundTaskStarter
import io.flutter.embedding.engine.FlutterEngine

/**
 * Registers the filestr/native channel on the foreground-service's
 * FlutterEngine as soon as it's created, so the service isolate (which spawns
 * the daemon) can resolve the host paths. flutter_foreground_task does not run
 * GeneratedPluginRegistrant on the service engine, so this hook is how the
 * channel becomes reachable there.
 */
class FgtEngineListener(context: Context) : FlutterForegroundTaskLifecycleListener {
    private val appContext = context.applicationContext

    override fun onEngineCreate(flutterEngine: FlutterEngine?) {
        val messenger = flutterEngine?.dartExecutor?.binaryMessenger ?: return
        NativePathsChannel.register(messenger, appContext)
    }

    override fun onTaskStart(starter: FlutterForegroundTaskStarter) {}

    override fun onTaskRepeatEvent() {}

    override fun onTaskDestroy() {}

    override fun onEngineWillDestroy() {}
}
