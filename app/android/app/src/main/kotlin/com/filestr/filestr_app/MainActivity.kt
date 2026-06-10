package com.filestr.filestr_app

import com.pravera.flutter_foreground_task.FlutterForegroundTaskPlugin
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine

class MainActivity : FlutterActivity() {
    companion object {
        // Guard against registering the service-engine listener more than once
        // (configureFlutterEngine runs again if the activity is recreated).
        private var lifecycleListenerRegistered = false
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)

        // On the UI engine, for NativeBridge.paths() in the UI isolate.
        NativePathsChannel.register(flutterEngine.dartExecutor.binaryMessenger, this)

        // And on the foreground-service engine, where the daemon is spawned.
        if (!lifecycleListenerRegistered) {
            lifecycleListenerRegistered = true
            FlutterForegroundTaskPlugin.addTaskLifecycleListener(FgtEngineListener(applicationContext))
        }
    }
}
