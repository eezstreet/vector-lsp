/// <reference path="./vector-lsp-plugin.d.ts" />

// Validates Item# fields in TreasureClassEx.txt.
//
// Each Item# cell has the form:   BASE[,KEY=VALUE[,KEY=VALUE...]]
//
// BASE must be one of:
//   1. An item code (weapons/armor/misc "code" column).
//   2. A Treasure Class name from another TreasureClassEx row, but ONLY if
//      that row appears ABOVE the current row in the file.
//   3. An auto-generated TC: any itemtypes "Code" where TreasureClass=1,
//      optionally followed by a positive integer level suffix
//      (e.g. "weap", "weap3", "armo6").
//   4. A uniqueitems "index" value.
//   5. A setitems "index" value.
//
// Valid modifier keys after the comma: mul cu cs cr cm ce cg ma mg

// ─── Constants ────────────────────────────────────────────────────────────────

const VALID_MOD_KEYS: Record<string, true> = {
    mul: true, cu: true, cs: true, cr: true,
    cm: true, ce: true, cg: true, ma: true, mg: true,
};

// ─── Helpers ──────────────────────────────────────────────────────────────────

// Split "BASE,key=val,key=val" → [base, "key=val,key=val"].
// Only splits when what follows the first comma looks like a modifier.
function splitModifiers(raw: string): [string, string] {
    const idx = raw.indexOf(",");
    if (idx !== -1) {
        const after = raw.slice(idx + 1);
        // After comma must start with a known modifier key followed by "="
        if (/^[a-z]+=/.test(after)) {
            return [raw.slice(0, idx).trim(), after];
        }
    }
    return [raw.trim(), ""];
}

function validateModifiers(modsStr: string): string | null {
    for (const part of modsStr.split(",")) {
        const eq = part.indexOf("=");
        if (eq === -1) return `Invalid modifier '${part}' (expected key=value)`;
        const key = part.slice(0, eq).trim();
        const val = part.slice(eq + 1).trim();
        if (!VALID_MOD_KEYS[key]) {
            return `Unknown modifier key '${key}' (valid: mul, cu, cs, cr, cm, ce, cg, ma, mg)`;
        }
        if (!val) return `Missing value for modifier '${key}'`;
    }
    return null;
}

// ─── validate ─────────────────────────────────────────────────────────────────

function validate(ctx: PluginContext): PluginDiagnostic[] {
    if (ctx.file !== "treasureclassex") return [];

    // ── Build TC-name → first-line-number map (all rows, case-insensitive key).
    // Store only the first occurrence so forward-reference detection is stable.
    const tcLineMap: Record<string, number> = {};
    for (const row of ctx.rows) {
        const name = (row["Treasure Class"] as string | undefined);
        if (name && name.trim()) {
            const key = name.trim().toLowerCase();
            if (!(key in tcLineMap)) {
                tcLineMap[key] = row.__line;
            }
        }
    }

    // ── Build set of auto-TC base codes from itemtypes where TreasureClass=1.
    const autoTcCodes = new Set(
        getFilteredColumnValues("itemtypes", "Code", "TreasureClass", "1")
            .map((c: string) => c.toLowerCase())
    );

    // ── Determine which external workspace files are present (guard against
    // false positives when running on an incomplete workspace).
    const hasWeapons    = hasFile("weapons");
    const hasArmor      = hasFile("armor");
    const hasMisc       = hasFile("misc");
    const hasUnique     = hasFile("uniqueitems");
    const hasSetitems   = hasFile("setitems");
    const hasItemSource = hasWeapons || hasArmor || hasMisc
                       || hasUnique  || hasSetitems
                       || autoTcCodes.size > 0;

    // ── Expand Item# columns until the workspace says a column is absent.
    const cols: string[] = [];
    for (let n = 1; ; n++) {
        const col = "Item" + n;
        if (!getColumn("treasureclassex", col)) break;
        cols.push(col);
    }

    const diags: PluginDiagnostic[] = [];

    for (const row of ctx.rows) {
        for (const col of cols) {
            const raw = row[col] as string | undefined;
            if (!raw || !raw.trim()) continue;

            const err = validateItem(
                raw.trim(), row.__line,
                tcLineMap, autoTcCodes,
                hasWeapons, hasArmor, hasMisc, hasUnique, hasSetitems, hasItemSource,
            );
            if (err) {
                const c = row.__colstarts[col] ?? 0;
                diags.push({
                    line:     row.__line,
                    col:      c,
                    endCol:   c + raw.length,
                    severity: "error",
                    message:  err,
                });
            }
        }
    }

    return diags;
}

function validateItem(
    raw: string,
    currentLine: number,
    tcLineMap: Record<string, number>,
    autoTcCodes: Set<string>,
    hasWeapons: boolean,
    hasArmor: boolean,
    hasMisc: boolean,
    hasUnique: boolean,
    hasSetitems: boolean,
    hasItemSource: boolean,
): string | null {
    // Strip surrounding double-quotes — the game engine requires quotes around
    // entries that contain modifiers (e.g. `"gld,mul=1280"`).
    if (raw.startsWith('"') && raw.endsWith('"')) {
        raw = raw.slice(1, -1).trim();
    }

    // ── Split BASE from optional modifiers.
    const [base, modsStr] = splitModifiers(raw);

    if (!base) return "Empty item value";

    // ── Validate modifiers (always, regardless of base validity).
    if (modsStr) {
        const modErr = validateModifiers(modsStr);
        if (modErr) return modErr;
    }

    const baseLower = base.toLowerCase();

    // ── 1. Item code (weapons / armor / misc).
    if ((hasWeapons && lookupKey("weapons", "code", base))
     || (hasArmor   && lookupKey("armor",   "code", base))
     || (hasMisc    && lookupKey("misc",    "code", base))) {
        return null;
    }

    // ── 2. Treasure Class name from another row ABOVE the current row.
    if (baseLower in tcLineMap) {
        const defLine = tcLineMap[baseLower];
        if (defLine >= currentLine) {
            return `'${base}' is a treasure class defined at or below the current row`
                 + ` (line ${defLine + 1}); TC references must point upward`;
        }
        return null;
    }

    // ── 3. Auto-generated TC: itemtype code (with TreasureClass=1), optionally
    //        followed by a positive integer level suffix (e.g. "weap", "weap3").
    if (autoTcCodes.size > 0) {
        if (autoTcCodes.has(baseLower)) return null;
        // Concatenated suffix: "weap3" → code="weap", level="3"
        const concatMatch = /^([a-z]+)([1-9][0-9]*)$/.exec(baseLower);
        if (concatMatch && autoTcCodes.has(concatMatch[1])) return null;
    }

    // ── 4. uniqueitems index.
    if (hasUnique && lookupKey("uniqueitems", "index", base)) return null;

    // ── 5. setitems index.
    if (hasSetitems && lookupKey("setitems", "index", base)) return null;

    // ── If no external item-source files are loaded at all we cannot determine
    // whether this is a valid item code or unique/set index, so skip rather than
    // emit a false positive.
    if (!hasItemSource) return null;

    return `'${base}' is not a valid item code, treasure class, auto-TC, unique index, or set item index`;
}

// ─── hover ─────────────────────────────────────────────────────────────────
function hover(ctx: HoverContext): HoverResult | null {
    if (!ctx.value) return null;
    if (ctx.file !== "treasureclassex") return null;
    if (!ctx.col.toLocaleLowerCase().startsWith("item")) return null;

    const names = getFilteredColumnValues("weapons", "name", "code", ctx.value).concat(
        getFilteredColumnValues("armor", "name", "code", ctx.value),
        getFilteredColumnValues("misc", "name", "code", ctx.value),
        getFilteredColumnValues("itemtypes", "name", "code", ctx.value.substring(0, ctx.value.length - 1))
    );
    const name = names[0];
    if (!name) return null;

    return { content: ctx.value + "\n\n" + name };
}