//! MCP Prompts for AimDB
//!
//! Provides contextual help and guidance to LLMs interacting with AimDB.

use crate::protocol::{Prompt, PromptMessage, PromptMessageContent};

/// Get all available prompts
pub fn list_prompts() -> Vec<Prompt> {
    vec![Prompt {
        name: "troubleshooting".to_string(),
        description: Some("Common issues and debugging steps for AimDB MCP server".to_string()),
        arguments: None,
    }]
}

/// Get a specific prompt by name
pub fn get_prompt(name: &str) -> Option<Vec<PromptMessage>> {
    match name {
        "troubleshooting" => Some(get_troubleshooting_prompt()),
        _ => None,
    }
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

### 3. Permission Denied Errors
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


## Getting Help

### Useful Tools
- `discover_instances` - Find all AimDB servers
- `get_instance_info` - Get detailed server information
- `list_records` - See all available records with metadata
- `drain_record` - Batch-read accumulated record values
- To wait for the next update after a prompt, use the CLI: `aimdb watch <record> --new --format json --count 1 --timeout <secs>` (see the aimdb-cli README, "Waiting for an event")

### Diagnostic Information
When reporting issues, include:
- MCP server version (`aimdb-mcp --version`)
- AimDB instance version (from get_instance_info)
- Socket path being used
- Error messages from logs
- Output of discover_instances
- Output of list_records for the instance

## Configuration Reference

### File Locations
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
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "troubleshooting");
    }

    #[test]
    fn test_get_troubleshooting_prompt() {
        let messages = get_prompt("troubleshooting");
        assert!(messages.is_some());
        let messages = messages.unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].content.text.contains("Common Issues"));
    }

    #[test]
    fn test_get_unknown_prompt() {
        let messages = get_prompt("unknown");
        assert!(messages.is_none());
    }
}
