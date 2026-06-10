// Client for the filestrd control socket: newline-delimited JSON over a unix
// domain socket, mirroring filestrctl. Requests are {"id":N,"body":{...}} and
// responses {"id":N,"body":{...}} with the same id. Most operations are a
// single request/response; search and get stream multiple responses ending in
// a terminal variant (search_done / get_done).

import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// A response body decoded from the daemon (the inner `body` object, with its
/// discriminating `type` field intact).
typedef Body = Map<String, dynamic>;

class ControlException implements Exception {
  final String message;
  ControlException(this.message);
  @override
  String toString() => 'filestrd: $message';
}

/// Connection to a running filestrd over its unix socket. Multiplexes requests
/// by id so concurrent calls and streaming responses don't interfere.
class ControlClient {
  final Socket _socket;
  int _nextId = 1;

  // Single-response calls: id -> completer for the first response.
  final Map<int, Completer<Body>> _pending = {};
  // Streaming calls: id -> sink that receives every response for that id.
  final Map<int, StreamController<Body>> _streams = {};

  ControlClient._(this._socket) {
    _socket
        .cast<List<int>>()
        .transform(utf8.decoder)
        .transform(const LineSplitter())
        .listen(_onLine, onError: _onError, onDone: _onDone);
  }

  static Future<ControlClient> connect(String socketPath) async {
    final addr = InternetAddress(socketPath, type: InternetAddressType.unix);
    final socket = await Socket.connect(addr, 0);
    return ControlClient._(socket);
  }

  void _onLine(String line) {
    if (line.trim().isEmpty) return;
    final Map<String, dynamic> msg;
    try {
      msg = jsonDecode(line) as Map<String, dynamic>;
    } catch (_) {
      return;
    }
    final id = msg['id'] as int?;
    final body = (msg['body'] as Map?)?.cast<String, dynamic>();
    if (id == null || body == null) return;

    final stream = _streams[id];
    if (stream != null) {
      stream.add(body);
      return;
    }
    final completer = _pending.remove(id);
    if (completer != null && !completer.isCompleted) {
      completer.complete(body);
    }
  }

  void _onError(Object e) {
    final err = ControlException('connection error: $e');
    for (final c in _pending.values) {
      if (!c.isCompleted) c.completeError(err);
    }
    _pending.clear();
    for (final s in _streams.values) {
      s.addError(err);
    }
  }

  void _onDone() {
    final err = ControlException('daemon closed the connection');
    for (final c in _pending.values) {
      if (!c.isCompleted) c.completeError(err);
    }
    _pending.clear();
    for (final s in _streams.values) {
      s.close();
    }
    _streams.clear();
  }

  void _send(int id, Body body) {
    _socket.add(utf8.encode('${jsonEncode({'id': id, 'body': body})}\n'));
  }

  /// Send a request and await a single response. Throws on an `error` body.
  Future<Body> call(Body body) async {
    final id = _nextId++;
    final completer = Completer<Body>();
    _pending[id] = completer;
    _send(id, body);
    final resp = await completer.future.timeout(
      const Duration(seconds: 30),
      onTimeout: () => throw ControlException('timed out waiting for response'),
    );
    if (resp['type'] == 'error') {
      throw ControlException(resp['message']?.toString() ?? 'unknown error');
    }
    return resp;
  }

  /// Send a streaming request; emits every response for this id. The caller is
  /// responsible for recognising the terminal variant and cancelling.
  Stream<Body> stream(Body body) {
    final id = _nextId++;
    final controller = StreamController<Body>(
      onCancel: () => _streams.remove(id),
    );
    _streams[id] = controller;
    _send(id, body);
    return controller.stream;
  }

  Future<void> close() async {
    await _socket.close();
  }

  // --- typed convenience wrappers over the protocol --------------------------

  Future<Body> status() => call({'type': 'status'});

  Future<String> inviteCreate({String? label, bool? noReshare}) async {
    final resp = await call({
      'type': 'invite_create',
      'label': ?label,
      if (noReshare != null) 'allow_reshare': !noReshare,
    });
    return resp['ticket'] as String;
  }

  Future<List<Body>> inviteList() async {
    final resp = await call({'type': 'invite_list'});
    return (resp['invites'] as List).cast<Body>();
  }

  Future<Body> peerAdd(String ticket, {String? label}) async {
    final resp = await call({
      'type': 'peer_add',
      'ticket': ticket,
      'label': ?label,
    });
    return resp['peer'] as Body;
  }

  /// Returns (grants, peers).
  Future<(List<Body>, List<Body>)> peerList() async {
    final resp = await call({'type': 'peer_list'});
    return (
      (resp['grants'] as List).cast<Body>(),
      (resp['peers'] as List).cast<Body>(),
    );
  }

  Future<List<Body>> browse(String peer) async {
    final resp = await call({'type': 'browse', 'peer': peer});
    return (resp['entries'] as List).cast<Body>();
  }

  Future<int> rescan() async {
    final resp = await call({'type': 'rescan'});
    return resp['files'] as int;
  }

  /// Search the grant graph; emits each hit, completes on search_done.
  Stream<Body> search(String query) async* {
    await for (final body in stream({'type': 'search', 'query': query})) {
      final type = body['type'];
      if (type == 'search_hit') {
        yield body['hit'] as Body;
      } else if (type == 'search_done' || type == 'error') {
        return;
      }
    }
  }

  /// Background fetch: returns the transfer id immediately.
  Future<int> getBackground(String hash, String out, {String? peer}) async {
    final resp = await call({
      'type': 'get',
      'hash': hash,
      'out': out,
      'background': true,
      'peer': ?peer,
    });
    return resp['id'] as int;
  }

  Future<List<Body>> transfers() async {
    final resp = await call({'type': 'transfers'});
    return (resp['transfers'] as List).cast<Body>();
  }

  Future<void> transferCancel(int id) =>
      call({'type': 'transfer_cancel', 'id': id});
}
