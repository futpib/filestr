// Unit tests for stripAnsi — the helper that makes daemon log output legible
// on the failed-start screen by removing ANSI colour escapes.

import 'package:flutter_test/flutter_test.dart';
import 'package:filestr_app/main.dart';

void main() {
  test('strips SGR colour escapes, keeps the text', () {
    // a real tracing-style line: dim timestamp, green level, dim target
    const line =
        '\x1B[2m2026-06-11T17:21:20Z\x1B[0m \x1B[32m INFO\x1B[0m '
        '\x1B[2mfilestrd\x1B[0m\x1B[2m:\x1B[0m control socket ready';
    expect(stripAnsi(line),
        '2026-06-11T17:21:20Z  INFO filestrd: control socket ready');
  });

  test('leaves plain text untouched', () {
    const plain = 'no escapes here: 127.0.0.1:11780';
    expect(stripAnsi(plain), plain);
  });

  test('handles multi-line input and back-to-back escapes', () {
    const input = '\x1B[33mWARN\x1B[0m\x1B[2m a\x1B[0m\nplain\n\x1B[31mERR\x1B[0m';
    expect(stripAnsi(input), 'WARN a\nplain\nERR');
  });
}
