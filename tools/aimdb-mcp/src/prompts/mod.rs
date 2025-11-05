//! MCP Prompts for AimDB
//!
//! Provides contextual help and guidance to LLMs interacting with AimDB.

use crate::protocol::{Prompt, PromptMessage, PromptMessageContent};
use std::path::Path;

/// Get all available prompts
pub fn list_prompts() -> Vec<Prompt> {
    vec![
        Prompt {
            name: "notification-directory".to_string(),
            description: Some(
                "Get information about the notification file directory location and usage"
                    .to_string(),
            ),
            arguments: None,
        },
        Prompt {
            name: "subscription-help".to_string(),
            description: Some(
                "Learn how to subscribe to AimDB records and analyze notification data".to_string(),
            ),
            arguments: None,
        },
        Prompt {
            name: "schema-help".to_string(),
            description: Some(
                "Learn how to query and interpret record schemas before setting values".to_string(),
            ),
            arguments: None,
        },
        Prompt {
            name: "troubleshooting".to_string(),
            description: Some("Common issues and debugging steps for AimDB MCP server".to_string()),
            arguments: None,
        },
    ]
}

/// Get a specific prompt by name
pub fn get_prompt(name: &str, notification_dir: &Path) -> Option<Vec<PromptMessage>> {
    match name {
        "notification-directory" => Some(get_notification_directory_prompt(notification_dir)),
        "subscription-help" => Some(get_subscription_help_prompt()),
        "schema-help" => Some(get_schema_help_prompt()),
        "troubleshooting" => Some(get_troubleshooting_prompt()),
        _ => None,
    }
}

/// Notification directory information prompt
fn get_notification_directory_prompt(notification_dir: &Path) -> Vec<PromptMessage> {
    let text = format!(
        r#"# AimDB Notification File Location

## Directory Location
All subscription notifications are automatically saved to:
**{}**

## File Naming Pattern
Files are organized as: `{{date}}__{{record_name}}.jsonl`

Examples:
- `2025-11-04__server__Temperature.jsonl`
- `2025-11-04__server__SystemStatus.jsonl`

## File Format
Each file contains JSONL (JSON Lines) format with one notification per line:

```json
{{"timestamp":1762209978409,"value":{{"celsius":325.0,"sensor_id":"sensor-02"}},"sequence":1}}
{{"timestamp":1762209980407,"value":{{"celsius":326.5,"sensor_id":"sensor-03"}},"sequence":2}}
```

## How to Use
1. **List available files**: Use `list_dir` on the notification directory
2. **Read recent data**: Use `read_file` to read notification files
3. **Analyze trends**: Parse JSONL format and analyze values over time
4. **Multi-day analysis**: Read multiple date files for historical trends

## Configuration
- Enable/disable: Set `AIMDB_MCP_NOTIFICATION_FILES=true/false`
- Change location: Set `AIMDB_MCP_NOTIFICATION_DIR=/custom/path`

## File Rotation
- New file created automatically each day per record
- No automatic cleanup (user responsibility)
- Safe to delete old files: `rm {}/2025-10-*`

## Tips
- Files are append-only (safe for concurrent reads)
- Use `tail -f` for real-time monitoring
- Process with standard tools: `jq`, `grep`, `awk`
- Each subscription maintains independent sequence numbers
"#,
        notification_dir.display(),
        notification_dir.display()
    );

    vec![PromptMessage {
        role: "user".to_string(),
        content: PromptMessageContent {
            content_type: "text".to_string(),
            text,
        },
    }]
}

/// Subscription help prompt
fn get_subscription_help_prompt() -> Vec<PromptMessage> {
    let text = r#"# How to Subscribe to AimDB Records

## Step-by-Step Guide

### 1. Discover AimDB Instances
```
Use tool: discover_instances
Returns: List of running AimDB servers with socket paths
```

### 2. List Available Records
```
Use tool: list_records
Parameters: socket_path (from step 1)
Returns: All records with metadata (buffer type, capacity, update times)
```

### 3. Subscribe to a Record
```
Use tool: subscribe_record
Parameters:
  - socket_path: Path to AimDB instance
  - record_name: Name of record to monitor
  - max_samples: Number of samples before auto-unsubscribe
    * 10-30 samples: Quick check (~20-60 seconds)
    * 50-100 samples: Short monitoring (~2-3 minutes)
    * 200-500 samples: Extended analysis (~7-17 minutes)
    * null: Unlimited (requires explicit user confirmation)

Returns: subscription_id
```

### 4. Analyze Notification Data
Notifications are automatically saved to files. Access them by:
```
1. Use the "notification-directory" prompt to find the directory
2. Use list_dir to see available notification files
3. Use read_file to read notification data
4. Parse JSONL format and analyze values
```

### 5. Unsubscribe (if needed)
```
Use tool: unsubscribe_record
Parameters: subscription_id (from step 3)
```
Note: With max_samples set, auto-unsubscribe happens automatically.

## Best Practices

### Always Ask for Sample Limits
- Never subscribe without getting user input on duration/sample count
- Unlimited subscriptions can fill disk space
- Suggest appropriate limits based on use case

### Sample Size Recommendations
- **Quick trend check**: 30-50 samples
- **Statistical analysis**: 100-200 samples
- **Long-term monitoring**: 500-1000 samples
- **Alerting/continuous**: Requires explicit "unlimited" confirmation

### Analyzing Data
- Read from notification files (not get_record tool)
- Process recent lines for latest trends
- Aggregate across multiple days for historical analysis
- Use timestamps to correlate events across records

## Common Patterns

### Temperature Monitoring
1. Subscribe to "server::Temperature" with max_samples=50
2. Wait for subscription to complete (auto-unsubscribe)
3. Read notification file and analyze last 50 readings
4. Report trends, anomalies, or statistics

### Multi-Record Correlation
1. Subscribe to multiple records (e.g., Temperature + SystemStatus)
2. Wait for completion
3. Read both notification files
4. Match timestamps to find correlations

### Alerting
1. Get explicit user confirmation for unlimited monitoring
2. Subscribe with max_samples=null
3. Periodically check notification file for threshold breaches
4. Alert user when condition is met
5. Unsubscribe when user requests
"#;

    vec![PromptMessage {
        role: "user".to_string(),
        content: PromptMessageContent {
            content_type: "text".to_string(),
            text: text.to_string(),
        },
    }]
}

/// Schema help prompt
fn get_schema_help_prompt() -> Vec<PromptMessage> {
    let text = r#"# Working with AimDB Record Schemas

## Using query_schema Tool

The `query_schema` tool helps you understand the structure and types of AimDB records before setting values.

### Step-by-Step Guide

#### 1. Query the Schema
```
Use tool: query_schema
Parameters:
  - socket_path: Path to AimDB instance
  - record_name: Name of the record (e.g., "server::Temperature")
  - include_example: true (default) - includes current value as example

Returns:
  - record_name: Name of the record
  - schema: JSON Schema describing the structure
  - metadata: Buffer info, producer/consumer counts, timestamps
  - example: Current value (if include_example=true)
  - inferred_at: When the schema was generated
```

#### 2. Interpret the Schema
The schema follows JSON Schema format:
- **type**: "object", "array", "string", "number", "integer", "boolean", "null"
- **properties**: For objects, lists all fields and their types
- **required**: Array of field names that must be provided
- **items**: For arrays, the type of array elements

#### 3. Use Field Names as Semantic Hints
Field names carry meaning - use them to infer units and formats:
- `celsius`, `fahrenheit` → Temperature units
- `timestamp`, `created_at`, `updated_at` → Unix timestamp (seconds since epoch)
- `sensor_id`, `user_id`, `device_id` → Identifier strings
- `percent`, `percentage` → Values 0-100
- `meters`, `kilometers`, `seconds` → Unit indicators

#### 4. Check the Example Value
The example shows a real value from the database:
- Use it as a template for the expected format
- See what values look like in practice
- Understand nested structure layout

## Best Practices

### Always Query Schema Before Setting Values
❌ **Bad**: Guess the format and try to set
```
User: "Set temperature to 75"
You: *Sets* {"temperature": 75} → ERROR!
```

✅ **Good**: Query schema first, then ask for clarification
```
User: "Set temperature to 75"
You: *Queries schema* → Sees field is called "celsius"
You: "I see the field is called 'celsius'. Did you mean 75°F (23.9°C) or 75°C?"
```

### Ask Users for Clarification on Units
When field names suggest units but input is ambiguous:

**Temperature Example**:
```
Schema has: {"celsius": {"type": "number"}}
User says: "Set temperature to 75"

Ask: "The 'celsius' field expects Celsius. Did you mean:
      - 75°F (approximately 23.9°C)?
      - Or 75°C directly?"
```

**Timestamp Example**:
```
Schema has: {"timestamp": {"type": "number"}}
User says: "Update the reading"

Ask: "Should I use the current time for the 'timestamp' field?
      (The example shows: 1730649600.123, which is a Unix timestamp)"
```

### Respect Required Fields
All fields in the `required` array must be provided:
```
Schema shows:
{
  "properties": {
    "celsius": {"type": "number"},
    "sensor_id": {"type": "string"},
    "timestamp": {"type": "number"}
  },
  "required": ["celsius", "sensor_id", "timestamp"]
}

When setting, you MUST provide all three fields.
Ask user for missing values if they didn't specify them.
```

### Check Source Code for Definitive Information
For production systems, suggest checking the Rust type definition:
```
"For authoritative information about units and constraints,
 I recommend checking the Rust source code where this record type is defined.
 The field names suggest 'celsius' is in Celsius, but the code will confirm."
```

## Common Patterns

### Setting a Record Value
1. Query schema to understand structure
2. Identify field names and types
3. Ask user to clarify ambiguous values (units, timestamps, etc.)
4. Confirm all required fields are provided
5. Use set_record with complete value

### Discovering Fields
```
User: "What fields does the SensorData record have?"

1. Query schema
2. List all properties with their types
3. Highlight required vs optional fields
4. Show example value for reference
```

### Type Validation
```
User tries to set: {"port": "8080"}
Schema shows: {"port": {"type": "integer"}}

Response: "Error: 'port' should be a number (integer), not a string.
           Try: {\"port\": 8080}"
```

## Understanding Schema Output

### Simple Types
```json
{
  "type": "integer"  // Whole numbers: 42, -10, 0
  "type": "number"   // Decimals: 3.14, -2.5, 42.0
  "type": "string"   // Text: "hello", "sensor-01"
  "type": "boolean"  // true or false
  "type": "null"     // null value
}
```

### Objects (Structs)
```json
{
  "type": "object",
  "properties": {
    "celsius": {"type": "number"},
    "sensor_id": {"type": "string"}
  },
  "required": ["celsius", "sensor_id"]
}
```
Means: `{"celsius": 22.5, "sensor_id": "sensor-01"}`

### Arrays
```json
{
  "type": "array",
  "items": {"type": "integer"}
}
```
Means: `[1, 2, 3, 4]`

### Nested Objects
```json
{
  "type": "object",
  "properties": {
    "sensor": {
      "type": "object",
      "properties": {
        "id": {"type": "string"},
        "location": {"type": "string"}
      }
    },
    "reading": {"type": "number"}
  }
}
```
Means: `{"sensor": {"id": "s1", "location": "Room A"}, "reading": 42.5}`

## When Schema Query Fails

### Record Not Found
If query_schema fails with "Record not found":
1. Use list_records to see available records
2. Check spelling - record names are case-sensitive
3. Include namespace prefix (e.g., "server::Temperature" not "Temperature")

### Connection Issues
If query_schema fails to connect:
1. Use discover_instances to find available instances
2. Verify socket path is correct
3. Check that AimDB instance is running

### Empty Array Items
If schema shows `"items": {}` for an array:
```
The array is currently empty - item type cannot be determined.
Ask user what type of items should be in the array,
or wait for the array to have values before querying schema.
```

## Tips

- **Query early**: Check schema before attempting to set values
- **Use examples**: The example value is your best guide
- **Ask, don't guess**: When units/formats are unclear, ask the user
- **Validate types**: Ensure numbers are numbers, strings are strings
- **Check metadata**: Producer/consumer counts tell you if record is active
- **Field names matter**: They're your primary hint about semantics

## Example Conversation

```
User: "Set the temperature to 72 degrees"

You: *Queries schema for "server::Temperature"*

Schema returns:
{
  "properties": {
    "celsius": {"type": "number"},
    "sensor_id": {"type": "string"},
    "timestamp": {"type": "number"}
  },
  "required": ["celsius", "sensor_id", "timestamp"],
  "example": {"celsius": 22.5, "sensor_id": "sensor-01", "timestamp": 1730649600.123}
}

You: "I can see the temperature record has these required fields:
     - celsius (number)
     - sensor_id (string)
     - timestamp (number)
     
     A few questions:
     1. Did you mean 72°F (which is 22.2°C) or 72°C directly?
     2. Which sensor? (The current example uses 'sensor-01')
     3. Should I use the current time for the timestamp?
     
     Once you clarify, I'll set the values."
```
"#;

    vec![PromptMessage {
        role: "user".to_string(),
        content: PromptMessageContent {
            content_type: "text".to_string(),
            text: text.to_string(),
        },
    }]
}

/// Troubleshooting prompt
fn get_troubleshooting_prompt() -> Vec<PromptMessage> {
    let text = r#"# AimDB MCP Server Troubleshooting

## Common Issues and Solutions

### 1. Cannot Find AimDB Instances
**Symptoms**: `discover_instances` returns empty list

**Solutions**:
- Check if AimDB processes are running: `ps aux | grep aimdb`
- Verify socket files exist in `/tmp/*.sock` or `/var/run/aimdb/*.sock`
- Ensure socket file permissions allow reading
- Check if socket paths are correct (not symbolic links)

### 2. Connection Timeout
**Symptoms**: `list_records` or other tools timeout

**Solutions**:
- Verify the socket path is correct
- Check if the AimDB instance is responsive
- Ensure no firewall blocking local socket connections
- Try reconnecting - connection may have been closed

### 3. Subscription Not Receiving Data
**Symptoms**: No notifications after subscribing

**Solutions**:
- Verify the record has producers: check `producer_count > 0` in list_records
- Confirm record is being updated: check `last_update` timestamp
- Ensure subscription was successful (check returned subscription_id)
- Check notification file is being written (list notification directory)

### 4. Notification Files Not Found
**Symptoms**: Cannot read notification files

**Solutions**:
- Use "notification-directory" prompt to get correct path
- Verify AIMDB_MCP_NOTIFICATION_FILES=true (default)
- Check file permissions on notification directory
- Ensure subscription has received at least one notification
- Look for files with correct naming: `{date}__{record}.jsonl`

### 5. Disk Space Issues
**Symptoms**: System running out of disk space

**Solutions**:
- Check notification file sizes: `du -sh ~/.aimdb-mcp/notifications/`
- Delete old notification files: `rm ~/.aimdb-mcp/notifications/2025-10-*`
- Set max_samples on subscriptions to prevent unbounded growth
- Unsubscribe from unused subscriptions
- Consider archiving old files: `gzip ~/.aimdb-mcp/notifications/2025-10-*`

### 6. Permission Denied Errors
**Symptoms**: Cannot read/write files or sockets

**Solutions**:
- Check file permissions on socket: `ls -la /tmp/*.sock`
- Verify user has read access to AimDB sockets
- Check notification directory permissions
- Run MCP server with appropriate user privileges

### 7. Invalid Record Names
**Symptoms**: Tools fail with "record not found"

**Solutions**:
- Use list_records to see exact record names (case-sensitive)
- Record names often have namespace prefixes (e.g., "server::Temperature")
- Don't use shortened names - full name required
- Check for typos in record names

## Debugging Commands

### Check MCP Server Status
- Look for log output in terminal
- Server logs initialization and all tool calls
- Check for error messages in logs

### Verify AimDB Instance
```bash
# List socket files
ls -la /tmp/*.sock /var/run/aimdb/*.sock

# Test socket connection
nc -U /tmp/aimdb-demo.sock  # Should connect
```

### Inspect Notification Files
```bash
# List notification directory
ls -lh ~/.aimdb-mcp/notifications/

# Check recent notifications
tail -n 5 ~/.aimdb-mcp/notifications/2025-11-04__server__Temperature.jsonl

# Count notifications
wc -l ~/.aimdb-mcp/notifications/*.jsonl

# Parse with jq
cat ~/.aimdb-mcp/notifications/2025-11-04__server__Temperature.jsonl | jq .
```

## Getting Help

### Useful Tools
- `discover_instances` - Find all AimDB servers
- `get_instance_info` - Get detailed server information
- `list_records` - See all available records with metadata
- `list_subscriptions` - Check active subscriptions

### Diagnostic Information
When reporting issues, include:
- MCP server version (`aimdb-mcp --version`)
- AimDB instance version (from get_instance_info)
- Socket path being used
- Error messages from logs
- Output of discover_instances
- Output of list_records for the instance

## Configuration Reference

### Environment Variables
- `AIMDB_MCP_NOTIFICATION_FILES` - Enable/disable file writing (default: true)
- `AIMDB_MCP_NOTIFICATION_DIR` - Notification directory (default: ~/.aimdb-mcp/notifications)

### File Locations
- Notification files: `~/.aimdb-mcp/notifications/*.jsonl`
- AimDB sockets: `/tmp/*.sock` or `/var/run/aimdb/*.sock`
- Config files: Project-specific locations
"#;

    vec![PromptMessage {
        role: "user".to_string(),
        content: PromptMessageContent {
            content_type: "text".to_string(),
            text: text.to_string(),
        },
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_prompts() {
        let prompts = list_prompts();
        assert_eq!(prompts.len(), 4);
        assert_eq!(prompts[0].name, "notification-directory");
        assert_eq!(prompts[1].name, "subscription-help");
        assert_eq!(prompts[2].name, "schema-help");
        assert_eq!(prompts[3].name, "troubleshooting");
    }

    #[test]
    fn test_get_notification_directory_prompt() {
        use std::path::PathBuf;
        let path = PathBuf::from("/test/path");
        let messages = get_prompt("notification-directory", &path);
        assert!(messages.is_some());
        let messages = messages.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert!(messages[0].content.text.contains("/test/path"));
    }

    #[test]
    fn test_get_subscription_help_prompt() {
        use std::path::PathBuf;
        let messages = get_prompt("subscription-help", &PathBuf::from("/tmp"));
        assert!(messages.is_some());
        let messages = messages.unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.text.contains("subscribe_record"));
    }

    #[test]
    fn test_get_troubleshooting_prompt() {
        use std::path::PathBuf;
        let messages = get_prompt("troubleshooting", &PathBuf::from("/tmp"));
        assert!(messages.is_some());
        let messages = messages.unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.text.contains("Common Issues"));
    }

    #[test]
    fn test_get_unknown_prompt() {
        use std::path::PathBuf;
        let messages = get_prompt("unknown", &PathBuf::from("/tmp"));
        assert!(messages.is_none());
    }
}
