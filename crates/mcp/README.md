# nuo-mcp

Model Context Protocol (MCP) client and server transport for Nuo.

## Overview

`nuo-mcp` implements the Anthropic Model Context Protocol (MCP) standard, enabling Nuo to connect to external out-of-process MCP servers and dynamically discover and invoke third-party tools over JSON-RPC 2.0.

It also supports running as an MCP server, allowing Nuo tools and capabilities to be consumed by external MCP-compliant hosts.

## Capabilities

- **MCP Client Transport**: Asynchronous `StdioTransport` launching and supervising external server processes with JSON-RPC 2.0 message framing.
- **Native Tool Adapter**: `McpNativeTool` transparently bridges MCP tool specifications into `nuo-tool::Tool`, preserving input schemas and error envelopes.
- **MCP Server Support**: `McpServer` exports local tools over stdio for external AI agent systems.
- **Lifecycle & Discovery**: Handles protocol version handshakes, capability negotiation, and tool list refreshes.
