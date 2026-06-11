// Thin bridge to the Android host for the few things Dart can't discover on
// its own: the directory native libraries are unpacked to (where our bundled
// `libfilestrd.so` daemon lives, executable) and the app's private storage
// dirs (which we hand to the daemon as its XDG roots).

import 'package:flutter/services.dart';

class NativePaths {
  /// nativeLibraryDir — contains the executable `libfilestrd.so`.
  final String nativeLibDir;

  /// App-private files dir (persistent): identity key, grants, config.
  final String filesDir;

  /// App-private cache dir: the blob store (regenerable by rescan).
  final String cacheDir;

  NativePaths({
    required this.nativeLibDir,
    required this.filesDir,
    required this.cacheDir,
  });

  String get daemonBinary => '$nativeLibDir/libfilestrd.so';
}

class NativeBridge {
  static const _channel = MethodChannel('filestr/native');

  static Future<NativePaths> paths() async {
    final m = (await _channel.invokeMapMethod<String, String>('getPaths'))!;
    return NativePaths(
      nativeLibDir: m['nativeLibDir']!,
      filesDir: m['filesDir']!,
      cacheDir: m['cacheDir']!,
    );
  }

  /// Launch Grayjay's Add Source flow for [url]. Returns false if Grayjay
  /// isn't installed (or the intent failed).
  static Future<bool> openInGrayjay(String url) async {
    return await _channel.invokeMethod<bool>('openInGrayjay', {'url': url}) ?? false;
  }
}
