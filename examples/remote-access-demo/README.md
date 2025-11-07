# Remote Access Demo

This example demonstrates the AimX v1 remote access protocol with `record.list` functionality.

## What It Does

**Server** (`server.rs`):
- Creates an AimDB instance with 4 record types (Temperature, SystemStatus, UserEvent, Config)
- Enables remote access on Unix domain socket `/tmp/aimdb-demo.sock`
- Uses ReadOnly security policy
- Populates some initial data

**Client** (`client.rs`):
- Connects to the server via Unix domain socket
- Performs protocol handshake (Hello/Welcome)
- Calls `record.list` method
- Displays all registered records with their metadata

## Running the Demo

### Terminal 1 - Start the Server

```bash
cargo run --example remote-access-demo --bin server
```

You should see:
```
🚀 Starting AimDB Remote Access Demo Server
📡 Remote access will be available at: /tmp/aimdb-demo.sock
🔒 Security policy: ReadOnly
✅ Database initialized with 4 record types
📝 Populated initial record data
🎯 Server ready!
```

### Terminal 2 - Run the Client

```bash
cargo run --example remote-access-demo --bin client
```

You should see:
```
🔌 Connecting to AimDB server...
✅ Connected!
📤 Sending handshake...
📥 Received welcome from server: aimdb
📤 Requesting record list...
✅ Success!
📋 Registered Records:
[
  {
    "name": "Temperature",
    "type_id": "...",
    "buffer_type": "none",
    "producer_count": 1,
    "consumer_count": 0,
    "writable": false,
    ...
  },
  ...
]
```

## Manual Testing with `socat`

You can also test manually using `socat`:

```bash
# Send handshake
echo '{"version":"1.0","client":"test"}' | socat - UNIX-CONNECT:/tmp/aimdb-demo.sock

# Send record.list request (after handshake)
(echo '{"version":"1.0","client":"test"}'; sleep 0.1; echo '{"id":1,"method":"record.list"}') | socat - UNIX-CONNECT:/tmp/aimdb-demo.sock
```

## What to Observe

- **Handshake**: Client sends Hello, server responds with Welcome
- **Permissions**: Server reports ReadOnly permissions
- **Record Metadata**: Each record shows:
  - Type name (Rust struct name)
  - TypeId (unique identifier)
  - Buffer configuration
  - Producer/consumer counts
  - Write permissions
  - Timestamps

## Next Steps

Future enhancements will add:
- `record.get` - Get current value
- `record.set` - Set writable records
- `record.subscribe` - Stream live updates
- `instance.shutdown` - Graceful shutdown
