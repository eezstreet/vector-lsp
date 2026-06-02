/// <reference path="./vector-lsp-plugin.d.ts" />

// Shows the enum table entry when hovering over a cell in a column that has
// a table defined in the schema (e.g. "op" in cubemain.txt, "srvstfunc" in
// skills.txt, "pSpell" in misc.txt).
//
// Table convention: column 0 is always the code that matches the cell value.
// Known header names drive layout; unknown layouts fall back to a generic view.

function hover(ctx: HoverContext): HoverResult | null {
    if (!ctx.value) return null;

    const table = getEnumTable(ctx.file, ctx.col);
    if (!table) return null;

    const { headers, rows } = table;
    if (rows.length === 0) return null;

    // Find the row whose code (column 0) matches the hovered cell value.
    const matched = rows.find((r) => r[0] === ctx.value);
    if (!matched) return null;

    // Identify well-known column indices by header name.
    let nameIdx = -1;
    let paramsIdx = -1;
    let descIdx = -1;

    for (let i = 1; i < headers.length; i++) {
        const h = headers[i].toLowerCase();
        if (h === "name") nameIdx = i;
        else if (h === "parameters" || h === "params") paramsIdx = i;
        else if (h === "description" || h === "desc") descIdx = i;
    }

    const parts: string[] = [];

    // Line 1: the code value itself.
    parts.push(ctx.value);

    // Line 2: function/enum name if present.
    if (nameIdx >= 0 && matched[nameIdx]) {
        parts.push(matched[nameIdx]);
    }

    // Blank line before body sections.
    parts.push("");

    if (paramsIdx >= 0 && matched[paramsIdx]) {
        const paramList = matched[paramsIdx]
            .split(/[\r\n]+/)
            .map((s: string) => s.trim())
            .filter((s: string) => s.length > 0)
            .join(", ");
        parts.push("**Parameters:** " + paramList);
    }

    if (descIdx >= 0 && matched[descIdx]) {
        parts.push(matched[descIdx]);
    }

    // Fallback for tables with none of the standard column names: show all
    // non-code columns with their header label.
    if (nameIdx < 0 && paramsIdx < 0 && descIdx < 0) {
        for (let i = 1; i < headers.length; i++) {
            if (matched[i]) {
                parts.push("**" + headers[i] + ":** " + matched[i]);
            }
        }
    }

    // Drop trailing blank lines.
    while (parts.length > 0 && parts[parts.length - 1] === "") {
        parts.pop();
    }

    if (parts.length === 0) return null;

    return { content: parts.join("\n") };
}
