# Pgquerymon - Postgresql Query Profiler

A sleek terminal-based monitoring tool for PostgreSQL queries executed through Npgsql in .NET applications. Npgsql Monitor provides real-time SQL query visualization with syntax highlighting, performance metrics, and an intuitive accordion-style interface for detailed query inspection.

This tool works in conjunction with the **NpgsqlLogger** NuGet package to capture and stream SQL queries from your .NET applications. Install the logger package in your .NET project using:

```bash
dotnet add package NpgsqlLogger --source https://www.nuget.org/packages/NpgsqlLogger/
```

The NpgsqlLogger package (available at https://github.com/larswise/NpgsqlTcpLogger) captures SQL queries executed by Npgsql and streams them over TCP to the monitor. Once configured, simply run `npgsql-mon` in your terminal to start monitoring your application's database activity in real-time.

## Features

- **Real-time SQL monitoring** - View queries as they execute in your application
- **Syntax highlighting** - SQL queries are beautifully highlighted for easy reading
- **Performance metrics** - Duration tracking with color-coded indicators (green for fast, yellow for moderate, red for slow)
- **HTTP context** - See which endpoints triggered specific queries
- **Batch query support** - Handles and displays batch SQL operations
- **Interactive navigation** - Accordion-style interface with vim-like keybindings
- **Query copying** - Copy formatted SQL queries to clipboard with 'y' key
- **Scroll mode** - Navigate through long queries with j/k and Ctrl+d/Ctrl+u

## Usage

1. Install the NpgsqlLogger package in your .NET application
2. Configure the logger to send queries to `localhost:6000`
3. Run `pgquerymon` to start the monitoring interface
4. Execute queries in your application and watch them appear in real-time

## Keybindings

- `j/k` or `↑/↓` - Navigate between queries
- `Enter` - Expand/collapse query details
- `l` - Enter scroll mode for long queries
- `h` - Exit scroll mode
- `y` - Copy current query to clipboard
- `c` - Clear screen (remove all log entries)
- `Ctrl+d/u` - Page down/up navigation
- `q` - Quit the application
