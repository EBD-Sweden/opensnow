# OpenSnow SQL Playground

A browser-based SQL playground for exploring telecom and banking datasets. No installation required -- everything runs in your browser using SQLite compiled to WebAssembly.

## Quick Start

### Option 1: Open directly

Double-click `index.html` or open it in your browser. Everything is self-contained in a single file.

### Option 2: Local HTTP server

```bash
cd playground
python3 -m http.server 8080
```

Then open http://localhost:8080 in your browser.

> **Note:** Some browsers restrict WASM loading from `file://` URLs. If you see errors, use the HTTP server method above.

## Features

- **Monaco Editor** with SQL syntax highlighting (same editor as VS Code)
- **SQLite WASM** engine -- queries run entirely in the browser
- **Pre-loaded sample data:**
  - `cdrs` -- 1,000 call detail records (voice, SMS, data, MMS)
  - `subscribers` -- 500 telecom subscribers across 8 regions
  - `transactions` -- 2,000 banking transactions
  - `accounts` -- 500 bank accounts across 8 branches
- **Example queries** in the sidebar -- click to run
- **Keyboard shortcut:** Ctrl+Enter (or Cmd+Enter) to execute
- Dark theme, resizable editor panel

## Deploying

### GitHub Pages

1. Push the `playground/` directory to your repo.
2. Go to **Settings > Pages** and set the source to the branch/folder containing `index.html`.
3. The playground will be available at `https://<user>.github.io/<repo>/playground/`.

### Vercel

```bash
cd playground
npx vercel --prod
```

Or connect the repo in the Vercel dashboard and set the root directory to `playground/`.

### Netlify

Drag and drop the `playground/` folder onto [app.netlify.com/drop](https://app.netlify.com/drop), or:

```bash
cd playground
npx netlify-cli deploy --prod --dir .
```

### Any static host

Copy `index.html` to any static file server. There are no other files or build steps required.

## Architecture

This playground uses **sql.js** (SQLite compiled to WASM) as a lightweight demo engine. The full OpenSnow engine is built on Apache DataFusion (Rust). A future iteration will compile the DataFusion-based query engine to WASM for full compatibility with OpenSnow's query dialect and features.

## Sample Tables Schema

```sql
-- Telecom
subscribers(id, phone, name, region, plan, signup_date)
cdrs(id, caller, callee, call_type, duration_sec, cost, ts)

-- Banking
accounts(id, holder_name, account_type, balance, branch, opened_date)
transactions(id, account_id, txn_type, amount, channel, ts)
```
