#!/usr/bin/env python3
"""Turn the "Krona's Bargain" dashboard into a step-by-step read.

Re-lays an operator-selected existing public dashboard with interleaved
**text cards** — an intro plus a "### Act N · …  👉 Look for: …" card before each
chart — matching the "How Europe Borrows & Spends — a step-by-step read" pattern.
Reuses the existing question cards (so the per-card public links the blog embeds
keep working) and preserves the dashboard UUID.

Env: MB_URL, MB_EMAIL, MB_PASSWORD, OPENSNOW_KRONA_PUBLIC_UUID
"""
import json, os, sys, urllib.request, urllib.error

MB = os.environ.get("MB_URL", "http://localhost:3000").rstrip("/")
EMAIL = os.environ["MB_EMAIL"]; PASSWORD = os.environ["MB_PASSWORD"]
PUBLIC_UUID = os.environ["OPENSNOW_KRONA_PUBLIC_UUID"]
S = None

def api(method, path, body=None):
    h = {"Content-Type": "application/json"}
    if S: h["X-Metabase-Session"] = S
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{MB}{path}", data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(req, timeout=40) as r:
            raw = r.read().decode(); return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        print(f"  ! {method} {path} -> {e.code}: {e.read().decode()[:300]}", file=sys.stderr); raise

def text_card(md, row, col, sx, sy, n):
    return {"id": -n, "card_id": None, "row": row, "col": col, "size_x": sx, "size_y": sy,
            "visualization_settings": {"text": md, "dashcard.background": False, "text.align_vertical": "middle"}}

def chart_card(cid, row, col, sx, sy, n):
    return {"id": -n, "card_id": cid, "row": row, "col": col, "size_x": sx, "size_y": sy,
            "visualization_settings": {}}

def main():
    global S
    S = api("POST", "/api/session", {"username": EMAIL, "password": PASSWORD})["id"]

    # Find the dashboard id + map its question cards by name keyword.
    dash_id = None
    for d in api("GET", "/api/dashboard"):
        if d.get("public_uuid") == PUBLIC_UUID:
            dash_id = d["id"]; break
    if not dash_id:
        print("dashboard not found", file=sys.stderr); sys.exit(1)
    full = api("GET", f"/api/dashboard/{dash_id}")
    cards = full.get("dashcards") or full.get("ordered_cards") or []
    by_kw = {}
    for c in cards:
        nm = ((c.get("card") or {}).get("name") or "").lower()
        cid = c.get("card_id")
        if not cid: continue
        for kw in ["weak krona", "real exchange", "unit labour", "indebted", "lost half", "public debt", "diversified saver"]:
            if kw in nm: by_kw[kw] = cid
    g = lambda kw: by_kw[kw]

    # Build the narrated layout (24-col grid). Stack rows as we go.
    payload = []; n = 0; row = 0
    def add_text(md, sy):
        nonlocal row, n
        n += 1; payload.append(text_card(md, row, 0, 24, sy, n)); row += sy
    def add_charts(pairs, sy=7):  # pairs: list of (card_id, size_x)
        nonlocal row, n; col = 0
        for cid, sx in pairs:
            n += 1; payload.append(chart_card(cid, row, col, sx, sy, n)); col += sx
        row += sy

    add_text(
        "## 🇸🇪 The Krona's Bargain — a step-by-step read\n"
        "**A weak currency is a wealth transfer.** Built on **OpenSnow** over Eurostat, ECB & market "
        "data (2010–2026). We follow Sweden's cheap krona through four doors — the firm, the household, "
        "the state, and the saver — to see who gained and who paid.", 3)

    add_text("### The setup · A weak krona\n"
             "👉 **Look for:** the steady climb — it takes about 20% more krona to buy a euro in 2023 than in 2010.", 2)
    add_charts([(g("weak krona"), 24)], 6)

    add_text("### Act 1 · The firm wins — but the currency did the work, not wages\n"
             "👉 **Look for:** Sweden's real exchange rate (REER, left) *falling* while Germany's rises — yet "
             "unit labour costs (right) rise alike. The krona, not wage restraint, delivered the competitiveness.", 3)
    add_charts([(g("real exchange"), 12), (g("unit labour"), 12)])

    add_text("### Act 2 · The household pays — through debt, not the supermarket\n"
             "👉 **Look for:** Sweden's high debt per person and thin cash buffer (left), and a lost half-decade "
             "of real income (right). Variable-rate mortgages met a sharp rate-hiking cycle.", 3)
    add_charts([(g("indebted"), 12), (g("lost half"), 12)])

    add_text("### Act 3 · The state holds — the buffer the euro-south lacks\n"
             "👉 **Look for:** Sweden & Denmark near 30–35% debt/GDP versus the euro-south above 100%. "
             "Owning your own currency *and* low debt = two shock absorbers.", 2)
    add_charts([(g("public debt"), 24)])

    add_text("### Payoff · The diversified saver won — by accident\n"
             "👉 **Look for:** Sweden's wealth index pulling ahead. A falling krona lifts foreign assets, and "
             "Swedish households hold far more equity than cash — so the saver was hedged against the very "
             "weakness that squeezed the borrower.", 3)
    add_charts([(g("diversified saver"), 24)])

    api("PUT", f"/api/dashboard/{dash_id}/cards", {"cards": payload})
    print(f"narrated dashboard {dash_id} ({len(payload)} cards) -> {MB}/public/dashboard/{PUBLIC_UUID}")

if __name__ == "__main__":
    main()
