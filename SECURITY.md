# Security Policy

## Supported Versions

Only the latest released version of AimDB receives security fixes.

| Version | Supported          |
| ------- | ------------------ |
| 1.x     | :white_check_mark: |
| < 1.0   | :x:                |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Report vulnerabilities privately using
[GitHub's private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing/privately-reporting-a-security-vulnerability)
via the **"Report a vulnerability"** button on the [Security tab](../../security/advisories/new) of this repository.

Include as much of the following as possible:

- Description of the vulnerability and its potential impact
- Affected component (e.g., `aimdb-core`, `aimdb-mqtt-connector`, `aimdb-mcp`)
- Steps to reproduce or proof-of-concept
- Suggested fix or mitigation, if you have one

## Response Timeline

| Milestone                        | Target        |
| -------------------------------- | ------------- |
| Initial acknowledgement          | Within 48 h   |
| Confirmed / triaged              | Within 7 days |
| Fix released (critical severity) | Within 14 days |
| Fix released (other severity)    | Within 30 days |

We will keep you informed of progress throughout the process.

## Disclosure Policy

We follow coordinated disclosure:

1. Reporter submits the vulnerability privately.
2. We confirm, triage, and develop a fix.
3. A patched release is published.
4. A GitHub Security Advisory is published after the fix is available.
5. Reporter is credited in the advisory unless they prefer to remain anonymous.

## Scope

### In scope

- Memory safety or logic errors in `aimdb-core`
- Authentication or authorization bypass in any connector (MQTT, KNX, WebSocket)
- Prompt injection or data exfiltration via the MCP server (`aimdb-mcp`)
- Denial-of-service vulnerabilities in network-facing components
- Dependency vulnerabilities with a direct exploit path in AimDB

### Out of scope

- Vulnerabilities in third-party dependencies without a direct exploit path
- Issues requiring physical access to a device running AimDB
- Social engineering or phishing
- Theoretical vulnerabilities without a proof of concept

## Security Considerations for Users

AimDB is designed for use in trusted environments (MCU → edge → cloud). A few
recommendations:

- **MQTT**: Enable TLS and use strong credentials in production deployments.
- **KNX**: Restrict network access to trusted KNX/IP segments.
- **MCP server**: Only expose the Unix socket to trusted local processes.
- **WebSocket connector**: Always run behind a TLS-terminating reverse proxy in
  production.
- Keep AimDB and its dependencies up to date. Run `cargo audit` regularly to
  check for known advisories in the dependency tree.
