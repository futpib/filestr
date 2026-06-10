// The foreground-service side: a TaskHandler that runs in its own isolate
// inside the foreground service, spawns the bundled filestrd daemon, and keeps
// it alive for as long as the service runs. The UI isolate never spawns the
// daemon — it just connects to the unix socket this handler's daemon listens
// on. This mirrors iroh-ssh-android, where the network session lives in the
// foreground-service isolate so it survives the app being backgrounded.

import 'dart:io';

import 'package:flutter/widgets.dart';
import 'package:flutter_foreground_task/flutter_foreground_task.dart';

import 'filestr_layout.dart';
import 'native_bridge.dart';

/// Entry point for the foreground-service isolate. Registered as the service
/// callback; flutter_foreground_task invokes it in the background isolate.
@pragma('vm:entry-point')
void startCallback() {
  FlutterForegroundTask.setTaskHandler(DaemonTaskHandler());
}

class DaemonTaskHandler extends TaskHandler {
  Process? _process;
  bool _stopping = false;

  @override
  Future<void> onStart(DateTime timestamp, TaskStarter starter) async {
    // The platform channel used by NativeBridge is registered on this service
    // engine by FgtEngineListener (native side).
    WidgetsFlutterBinding.ensureInitialized();
    try {
      await _spawn();
      FlutterForegroundTask.updateService(
        notificationTitle: 'filestr',
        notificationText: 'Sharing — daemon running',
      );
    } catch (e) {
      FlutterForegroundTask.updateService(
        notificationTitle: 'filestr',
        notificationText: 'Daemon failed: $e',
      );
    }
  }

  Future<void> _spawn() async {
    final paths = await NativeBridge.paths();
    final layout = FilestrLayout(paths);
    await layout.writeConfig();

    // Remove a stale socket from a previous run so bind() succeeds.
    final sock = File(layout.socketPath);
    if (await sock.exists()) await sock.delete();

    final proc = await Process.start(
      layout.daemonBinary,
      ['--config', layout.configPath, '--socket', layout.socketPath],
      environment: layout.env(),
      // Run inside the sandbox: the inherited CWD is a directory the daemon
      // isn't allowed to traverse on Android.
      workingDirectory: paths.filesDir,
    );
    _process = proc;

    final log = File(layout.logPath).openWrite();
    log.writeln('--- daemon started ---');
    proc.stdout.transform(const SystemEncoding().decoder).listen(log.write);
    proc.stderr.transform(const SystemEncoding().decoder).listen(log.write);

    // If the daemon dies while the service is meant to be running, restart it.
    proc.exitCode.then((code) {
      log.writeln('--- daemon exited: $code ---');
      if (!_stopping) {
        Future.delayed(const Duration(seconds: 1), () {
          if (!_stopping) _spawn();
        });
      }
    });
  }

  @override
  void onRepeatEvent(DateTime timestamp) {}

  @override
  Future<void> onDestroy(DateTime timestamp, bool isTimeout) async {
    _stopping = true;
    _process?.kill(ProcessSignal.sigterm);
    _process = null;
  }
}
