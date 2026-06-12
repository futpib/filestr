// UI-side handle to the daemon. The daemon itself is spawned and supervised by
// the foreground service (see daemon_runner.dart); here we only start that
// service and connect a ControlClient to the unix socket it brings up.

import 'dart:async';
import 'dart:io';

import 'package:flutter_foreground_task/flutter_foreground_task.dart';

import 'control_client.dart';
import 'daemon_runner.dart';
import 'filestr_layout.dart';
import 'native_bridge.dart';

class Daemon {
  final FilestrLayout layout;
  ControlClient? _client;

  Daemon(this.layout);

  String get socketPath => layout.socketPath;
  String get shareDir => layout.shareDir;
  String get downloadsDir => layout.downloadsDir;

  /// URL to add this node as a source in Grayjay (on this device).
  String get grayjayUrl =>
      'http://127.0.0.1:${FilestrLayout.httpPort}/grayjay/FilestrConfig.json';

  ControlClient get client {
    final c = _client;
    if (c == null) throw StateError('not connected');
    return c;
  }

  /// Configure + start the foreground service (idempotent).
  static void _configureForegroundTask() {
    FlutterForegroundTask.init(
      androidNotificationOptions: AndroidNotificationOptions(
        channelId: 'filestr_daemon',
        channelName: 'filestr daemon',
        channelDescription: 'Keeps the filestr daemon running for file sharing',
        channelImportance: NotificationChannelImportance.LOW,
        priority: NotificationPriority.LOW,
      ),
      iosNotificationOptions: const IOSNotificationOptions(),
      foregroundTaskOptions: ForegroundTaskOptions(
        eventAction: ForegroundTaskEventAction.nothing(),
        autoRunOnBoot: false,
        autoRunOnMyPackageReplaced: true,
        allowWakeLock: true,
        allowWifiLock: true,
      ),
    );
  }

  /// Start the daemon (via the foreground service) and connect a client.
  static Future<Daemon> startAndConnect() async {
    final paths = await NativeBridge.paths();
    final layout = FilestrLayout(paths);
    final daemon = Daemon(layout);

    _configureForegroundTask();
    await FlutterForegroundTask.requestNotificationPermission();

    if (!await FlutterForegroundTask.isRunningService) {
      await FlutterForegroundTask.startService(
        serviceTypes: const [ForegroundServiceTypes.dataSync],
        notificationTitle: 'filestr',
        notificationText: 'Starting daemon…',
        // The notification is ongoing (can't be swiped away on most Android
        // versions) so the daemon isn't left running invisibly; a Stop button
        // is the explicit way to shut it down. Mirrors iroh-ssh-android.
        notificationButtons: const [
          NotificationButton(id: 'stop', text: 'Stop'),
        ],
        callback: startCallback,
      );
    }

    daemon._client = await daemon._connectWithRetry();
    return daemon;
  }

  // How long to wait for the daemon to bring up its control socket. A real
  // device cold-starting the service isolate (and the user dismissing the
  // notification-permission dialog) can take well over ten seconds, so give it
  // a generous window before declaring failure.
  static const _connectAttempts = 300; // 300 * 100ms = 30s

  Future<ControlClient> _connectWithRetry() async {
    Object? lastErr;
    for (var i = 0; i < _connectAttempts; i++) {
      if (await File(socketPath).exists()) {
        try {
          return await ControlClient.connect(socketPath);
        } catch (e) {
          lastErr = e;
        }
      }
      await Future.delayed(const Duration(milliseconds: 100));
    }
    // Surface the daemon's own log tail to make failures actionable.
    var tail = '';
    try {
      final lines = await File(layout.logPath).readAsLines();
      tail = lines.length > 40 ? lines.sublist(lines.length - 40).join('\n') : lines.join('\n');
    } catch (_) {}
    throw ControlException(
        'daemon did not come up: ${lastErr ?? 'socket never appeared'}'
        '${tail.isNotEmpty ? '\n\n$tail' : ''}');
  }

  Future<void> close() async {
    await _client?.close();
    _client = null;
  }
}
