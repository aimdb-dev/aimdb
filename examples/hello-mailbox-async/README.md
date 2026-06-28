# hello-mailbox-async: async Mailbox buffer demo
The Mailbox buffer keeps only the last message written. It's like a real mailbox where only the most recent letter is visible.

## When to use
This Mailbox buffer demo is useful in a variety of scenarios where you want to retain only the last message, for example:
- When you have a robotic arm that needs to execute a sequence of commands, and you only want to execute the last command, or even if the robot gets too much commands in a short period of time.

## How it works
The Mailbox buffer demo works by simulating a mailbox buffer that retains only the last message using async API.
It sends 3 Colors and the MailBox only gets the last message.

## How to run
From the workspace root, run:
```
cargo run -p hello-mailbox-async
```
**Expected output**
```
=== hello-mailbox-async: Mailbox buffer demo ===

Firing three rapid commands BEFORE consumer exists: Red → Green → Blue
   ✓ Got: Blue  ← only the latest survived
Shutting down...
   ✓ Done.
```