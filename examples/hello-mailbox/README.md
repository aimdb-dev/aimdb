# hello-mailbox: Mailbox buffer demo
The Mailbox buffer keeps only the last message written. It's like a real mailbox where only the most recent letter is visible.

## When to use
This Mailbox buffer demo is useful in a variety of scenarios where you want to retain only the last message, for example:
- When you have a robotic arm that needs to execute a sequence of commands, and you only want to execute the last command, or even if the robot gets too much commands in a short period of time.

## How it works
The Mailbox buffer demo works by simulating a mailbox buffer that retains only the last message.
it runs 2 rounds:
  - the first round sends 3 Colors and the MailBox only gets the last message.
  - the second round sends 2 Colors and the MailBox only gets the last message.

## How to run
From the workspace root, run:
```
cargo run -p hello-mailbox
```
**Expected output**
```
=== hello-mailbox: Mailbox buffer demo ===

   Round 1
1. Firing three rapid commands BEFORE consumer exists: Red → Green → Blue
2. Consumer created AFTER the burst — reads once:
   ✓ Got: Blue  ← only the latest survived
   (Red and Green were overwritten before anyone could read them)

   Round 2
1. Firing two rapid commands BEFORE consumer exists: Red → Green
2. Consumer created AFTER the burst — reads once:
   ✓ Got: Green  ← only the latest survived (Green)
3. Shutting down...
   ✓ Done.
```
