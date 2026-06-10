// filestr — friend-to-friend file sharing, Android app (files only).
//
// The app bundles the iroh-only filestrd daemon (no chat/nostr) as a native
// library, spawns it on launch, and drives it over its unix-socket control
// protocol. The UI exposes the file-peering surface: identity/status, invites,
// peers + browse, grant-graph search, and downloads.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_foreground_task/flutter_foreground_task.dart';

import 'control_client.dart';
import 'daemon.dart';

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  // Required so the UI isolate can talk to the foreground-service isolate.
  FlutterForegroundTask.initCommunicationPort();
  runApp(const FilestrApp());
}

class FilestrApp extends StatelessWidget {
  const FilestrApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'filestr',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: Colors.deepPurple,
          brightness: Brightness.dark,
        ),
        useMaterial3: true,
      ),
      home: const WithForegroundTask(child: Boot()),
    );
  }
}

/// Starts the daemon, then shows the home screen (or an error with retry).
class Boot extends StatefulWidget {
  const Boot({super.key});
  @override
  State<Boot> createState() => _BootState();
}

class _BootState extends State<Boot> {
  Daemon? _daemon;
  Object? _error;

  @override
  void initState() {
    super.initState();
    _boot();
  }

  Future<void> _boot() async {
    setState(() => _error = null);
    try {
      final daemon = await Daemon.startAndConnect();
      // Index whatever is in the shared dir.
      await daemon.client.rescan();
      setState(() => _daemon = daemon);
    } catch (e) {
      setState(() => _error = e);
    }
  }

  @override
  Widget build(BuildContext context) {
    if (_error != null) {
      return Scaffold(
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                const Icon(Icons.error_outline, size: 48),
                const SizedBox(height: 16),
                Text('Could not start the daemon:\n$_error',
                    textAlign: TextAlign.center),
                const SizedBox(height: 16),
                FilledButton(onPressed: _boot, child: const Text('Retry')),
              ],
            ),
          ),
        ),
      );
    }
    final daemon = _daemon;
    if (daemon == null) {
      return const Scaffold(
        body: Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              CircularProgressIndicator(),
              SizedBox(height: 16),
              Text('Starting filestr…'),
            ],
          ),
        ),
      );
    }
    return Home(daemon: daemon);
  }
}

class Home extends StatefulWidget {
  final Daemon daemon;
  const Home({super.key, required this.daemon});
  @override
  State<Home> createState() => _HomeState();
}

class _HomeState extends State<Home> {
  int _tab = 0;

  @override
  Widget build(BuildContext context) {
    final d = widget.daemon;
    final pages = [
      StatusPage(daemon: d),
      PeersPage(daemon: d),
      SearchPage(daemon: d),
      TransfersPage(daemon: d),
    ];
    return Scaffold(
      appBar: AppBar(title: const Text('filestr')),
      body: pages[_tab],
      bottomNavigationBar: NavigationBar(
        selectedIndex: _tab,
        onDestinationSelected: (i) => setState(() => _tab = i),
        destinations: const [
          NavigationDestination(icon: Icon(Icons.dashboard), label: 'Status'),
          NavigationDestination(icon: Icon(Icons.people), label: 'Peers'),
          NavigationDestination(icon: Icon(Icons.search), label: 'Search'),
          NavigationDestination(
              icon: Icon(Icons.download), label: 'Transfers'),
        ],
      ),
    );
  }
}

// --- Status -----------------------------------------------------------------

class StatusPage extends StatefulWidget {
  final Daemon daemon;
  const StatusPage({super.key, required this.daemon});
  @override
  State<StatusPage> createState() => _StatusPageState();
}

class _StatusPageState extends State<StatusPage> {
  Body? _status;
  Object? _error;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    try {
      final s = await widget.daemon.client.status();
      setState(() {
        _status = s['status'] as Body;
        _error = null;
      });
    } catch (e) {
      setState(() => _error = e);
    }
  }

  Future<void> _rescan() async {
    final n = await widget.daemon.client.rescan();
    if (mounted) {
      ScaffoldMessenger.of(context)
          .showSnackBar(SnackBar(content: Text('Indexed $n files')));
    }
    await _refresh();
  }

  Future<void> _createInvite() async {
    try {
      final ticket = await widget.daemon.client.inviteCreate();
      if (!mounted) return;
      await showTicketDialog(context, 'Invitation ticket', ticket,
          'Send this to a friend. They paste it under Peers → Add peer to pair with you.');
    } catch (e) {
      _snack('$e');
    }
  }

  void _snack(String m) {
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(m)));
    }
  }

  @override
  Widget build(BuildContext context) {
    final s = _status;
    return RefreshIndicator(
      onRefresh: _refresh,
      child: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          if (_error != null)
            Card(
              color: Theme.of(context).colorScheme.errorContainer,
              child: ListTile(
                leading: const Icon(Icons.error_outline),
                title: Text('$_error'),
              ),
            ),
          if (s != null) ...[
            _kv('Node ID', s['endpoint_id']?.toString() ?? '—', copyable: true),
            _kv('Version', s['version']?.toString() ?? '—'),
            _kv('Shared files', '${s['files'] ?? 0}'),
            _kv('Peers', '${s['peers'] ?? 0}'),
            _kv('Active grants', '${s['grants_active'] ?? 0}'),
            _kv('Pending grants', '${s['grants_issued'] ?? 0}'),
          ],
          const SizedBox(height: 16),
          FilledButton.icon(
            onPressed: _createInvite,
            icon: const Icon(Icons.person_add),
            label: const Text('Create invitation'),
          ),
          const SizedBox(height: 8),
          OutlinedButton.icon(
            onPressed: _rescan,
            icon: const Icon(Icons.refresh),
            label: const Text('Rescan shared folder'),
          ),
          const SizedBox(height: 24),
          Text('Shared folder:\n${widget.daemon.shareDir}',
              style: Theme.of(context).textTheme.bodySmall),
          const SizedBox(height: 8),
          Text('Downloads:\n${widget.daemon.downloadsDir}',
              style: Theme.of(context).textTheme.bodySmall),
        ],
      ),
    );
  }

  Widget _kv(String k, String v, {bool copyable = false}) {
    return ListTile(
      title: Text(k),
      subtitle: Text(v, style: const TextStyle(fontFamily: 'monospace')),
      trailing: copyable
          ? IconButton(
              icon: const Icon(Icons.copy, size: 18),
              onPressed: () {
                Clipboard.setData(ClipboardData(text: v));
                _snack('Copied');
              },
            )
          : null,
    );
  }
}

// --- Peers ------------------------------------------------------------------

class PeersPage extends StatefulWidget {
  final Daemon daemon;
  const PeersPage({super.key, required this.daemon});
  @override
  State<PeersPage> createState() => _PeersPageState();
}

class _PeersPageState extends State<PeersPage> {
  List<Body> _peers = [];
  Object? _error;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    try {
      final (_, peers) = await widget.daemon.client.peerList();
      setState(() {
        _peers = peers;
        _error = null;
      });
    } catch (e) {
      setState(() => _error = e);
    }
  }

  Future<void> _addPeer() async {
    final ticket = await promptText(
      context,
      title: 'Add peer',
      hint: 'Paste a filestr1… ticket',
    );
    if (ticket == null || ticket.trim().isEmpty) return;
    try {
      await widget.daemon.client.peerAdd(ticket.trim());
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('Peer added')));
      }
      await _refresh();
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text('$e')));
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: RefreshIndicator(
        onRefresh: _refresh,
        child: _peers.isEmpty
            ? ListView(
                children: [
                  const SizedBox(height: 120),
                  Center(
                    child: Text(
                      _error != null
                          ? '$_error'
                          : 'No peers yet.\nAdd one with a filestr1… ticket.',
                      textAlign: TextAlign.center,
                    ),
                  ),
                ],
              )
            : ListView.builder(
                itemCount: _peers.length,
                itemBuilder: (context, i) {
                  final p = _peers[i];
                  final id = p['node_id']?.toString() ?? '';
                  final label = p['label']?.toString();
                  final shortId =
                      id.length > 16 ? '${id.substring(0, 16)}…' : id;
                  return ListTile(
                    leading: const Icon(Icons.computer),
                    title: Text(label ?? shortId),
                    subtitle: Text(id,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: const TextStyle(fontFamily: 'monospace')),
                    trailing: const Icon(Icons.chevron_right),
                    onTap: () => Navigator.of(context).push(
                      MaterialPageRoute(
                        builder: (_) => BrowsePage(
                            daemon: widget.daemon, peer: id, label: label),
                      ),
                    ),
                  );
                },
              ),
      ),
      floatingActionButton: FloatingActionButton.extended(
        onPressed: _addPeer,
        icon: const Icon(Icons.add),
        label: const Text('Add peer'),
      ),
    );
  }
}

class BrowsePage extends StatefulWidget {
  final Daemon daemon;
  final String peer;
  final String? label;
  const BrowsePage(
      {super.key, required this.daemon, required this.peer, this.label});
  @override
  State<BrowsePage> createState() => _BrowsePageState();
}

class _BrowsePageState extends State<BrowsePage> {
  List<Body> _entries = [];
  bool _loading = true;
  Object? _error;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    setState(() => _loading = true);
    try {
      final e = await widget.daemon.client.browse(widget.peer);
      setState(() {
        _entries = e;
        _loading = false;
        _error = null;
      });
    } catch (e) {
      setState(() {
        _loading = false;
        _error = e;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text(widget.label ?? 'Files')),
      body: _loading
          ? const Center(child: CircularProgressIndicator())
          : _error != null
              ? Center(child: Text('$_error'))
              : _entries.isEmpty
                  ? const Center(child: Text('No files shared'))
                  : ListView.builder(
                      itemCount: _entries.length,
                      itemBuilder: (context, i) {
                        final e = _entries[i];
                        return FileTile(
                          daemon: widget.daemon,
                          name: e['path']?.toString() ?? '',
                          size: (e['size'] as num?)?.toInt() ?? 0,
                          hash: e['hash']?.toString() ?? '',
                          peer: widget.peer,
                        );
                      },
                    ),
    );
  }
}

// --- Search -----------------------------------------------------------------

class SearchPage extends StatefulWidget {
  final Daemon daemon;
  const SearchPage({super.key, required this.daemon});
  @override
  State<SearchPage> createState() => _SearchPageState();
}

class _SearchPageState extends State<SearchPage> {
  final _controller = TextEditingController();
  final List<Body> _hits = [];
  bool _searching = false;

  Future<void> _run() async {
    final q = _controller.text.trim();
    if (q.isEmpty) return;
    setState(() {
      _hits.clear();
      _searching = true;
    });
    try {
      await for (final hit in widget.daemon.client.search(q)) {
        if (!mounted) return;
        setState(() => _hits.add(hit));
      }
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text('$e')));
      }
    } finally {
      if (mounted) setState(() => _searching = false);
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.all(12),
          child: TextField(
            controller: _controller,
            textInputAction: TextInputAction.search,
            onSubmitted: (_) => _run(),
            decoration: InputDecoration(
              hintText: 'Search the grant graph',
              border: const OutlineInputBorder(),
              suffixIcon: IconButton(
                icon: _searching
                    ? const SizedBox(
                        width: 18,
                        height: 18,
                        child: CircularProgressIndicator(strokeWidth: 2))
                    : const Icon(Icons.search),
                onPressed: _searching ? null : _run,
              ),
            ),
          ),
        ),
        Expanded(
          child: _hits.isEmpty
              ? Center(
                  child: Text(_searching ? 'Searching…' : 'No results'),
                )
              : ListView.builder(
                  itemCount: _hits.length,
                  itemBuilder: (context, i) {
                    final h = _hits[i];
                    return FileTile(
                      daemon: widget.daemon,
                      name: h['name']?.toString() ?? '',
                      size: (h['size'] as num?)?.toInt() ?? 0,
                      hash: h['hash']?.toString() ?? '',
                    );
                  },
                ),
        ),
      ],
    );
  }
}

// --- Transfers --------------------------------------------------------------

class TransfersPage extends StatefulWidget {
  final Daemon daemon;
  const TransfersPage({super.key, required this.daemon});
  @override
  State<TransfersPage> createState() => _TransfersPageState();
}

class _TransfersPageState extends State<TransfersPage> {
  List<Body> _transfers = [];

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    try {
      final t = await widget.daemon.client.transfers();
      if (mounted) setState(() => _transfers = t);
    } catch (_) {}
  }

  @override
  Widget build(BuildContext context) {
    return RefreshIndicator(
      onRefresh: _refresh,
      child: _transfers.isEmpty
          ? ListView(
              children: const [
                SizedBox(height: 120),
                Center(child: Text('No transfers.\nPull to refresh.')),
              ],
            )
          : ListView.builder(
              itemCount: _transfers.length,
              itemBuilder: (context, i) {
                final t = _transfers[i];
                final total = (t['total'] as num?)?.toInt() ?? 0;
                final done = (t['transferred'] as num?)?.toInt() ?? 0;
                final status = t['status']?.toString() ?? '';
                final out = t['out']?.toString() ?? '';
                final progress =
                    total > 0 ? (done / total).clamp(0.0, 1.0) : null;
                return ListTile(
                  title: Text(out.split('/').last,
                      maxLines: 1, overflow: TextOverflow.ellipsis),
                  subtitle: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      const SizedBox(height: 4),
                      LinearProgressIndicator(
                          value: status == 'done' ? 1.0 : progress),
                      const SizedBox(height: 4),
                      Text('$status · ${fmtBytes(done)} / ${fmtBytes(total)}'),
                    ],
                  ),
                  trailing: (status == 'active' || status == 'queued')
                      ? IconButton(
                          icon: const Icon(Icons.cancel),
                          onPressed: () async {
                            await widget.daemon.client
                                .transferCancel((t['id'] as num).toInt());
                            await _refresh();
                          },
                        )
                      : null,
                );
              },
            ),
    );
  }
}

// --- Shared widgets / helpers -----------------------------------------------

/// A file row (used by browse + search) with a download action.
class FileTile extends StatelessWidget {
  final Daemon daemon;
  final String name;
  final int size;
  final String hash;
  final String? peer;
  const FileTile({
    super.key,
    required this.daemon,
    required this.name,
    required this.size,
    required this.hash,
    this.peer,
  });

  Future<void> _download(BuildContext context) async {
    final base = name.split('/').last;
    final out = '${daemon.downloadsDir}/$base';
    try {
      await daemon.client.getBackground(hash, out, peer: peer);
      if (context.mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(content: Text('Downloading $base — see Transfers')));
      }
    } catch (e) {
      if (context.mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text('$e')));
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return ListTile(
      leading: const Icon(Icons.insert_drive_file),
      title: Text(name.split('/').last,
          maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Text(fmtBytes(size)),
      trailing: IconButton(
        icon: const Icon(Icons.download),
        onPressed: () => _download(context),
      ),
    );
  }
}

String fmtBytes(int n) {
  if (n <= 0) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  var v = n.toDouble();
  var u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return '${v.toStringAsFixed(u == 0 ? 0 : 1)} ${units[u]}';
}

Future<String?> promptText(BuildContext context,
    {required String title, String? hint}) {
  final controller = TextEditingController();
  return showDialog<String>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(title),
      content: TextField(
        controller: controller,
        autofocus: true,
        maxLines: null,
        decoration: InputDecoration(hintText: hint),
      ),
      actions: [
        TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Cancel')),
        FilledButton(
          onPressed: () => Navigator.pop(context, controller.text),
          child: const Text('OK'),
        ),
      ],
    ),
  );
}

Future<void> showTicketDialog(
    BuildContext context, String title, String ticket, String help) {
  return showDialog<void>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(title),
      content: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(help),
          const SizedBox(height: 12),
          Container(
            padding: const EdgeInsets.all(8),
            decoration: BoxDecoration(
              color: Theme.of(context).colorScheme.surfaceContainerHighest,
              borderRadius: BorderRadius.circular(8),
            ),
            child: SelectableText(ticket,
                style: const TextStyle(fontFamily: 'monospace', fontSize: 12)),
          ),
        ],
      ),
      actions: [
        TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Close')),
        FilledButton.icon(
          icon: const Icon(Icons.copy),
          label: const Text('Copy'),
          onPressed: () {
            Clipboard.setData(ClipboardData(text: ticket));
            Navigator.pop(context);
            ScaffoldMessenger.of(context)
                .showSnackBar(const SnackBar(content: Text('Copied ticket')));
          },
        ),
      ],
    ),
  );
}
