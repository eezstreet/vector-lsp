/// <reference path="../../vector-lsp-plugin.d.ts" />

// Shows the drop probability for the paired Item# when hovering over a Prob#
// cell in TreasureClassEx.txt.
//
// Formula (when Picks is positive):
//   total     = NoDrop + Prob1 + Prob2 + … + Prob10
//   per_roll  = ProbX / total
//   at_least_one = 1 − (1 − per_roll)^Picks   [only shown when Picks > 1]

function hover(ctx: HoverContext): HoverResult | null {
    if (ctx.file !== "treasureclassex") return null;

    const probMatch = /^prob(\d+)$/i.exec(ctx.col);
    if (!probMatch) return null;

    const cellVal = ctx.value.trim();
    if (!cellVal || cellVal === "0") return null;

    const prob = parseInt(cellVal, 10);
    if (isNaN(prob) || prob <= 0) return null;

    // Picks defaults to 1 when empty; reject negative (quantity mode, not probability).
    const picksRaw = ctx.row["Picks"];
    const picks = (picksRaw && picksRaw.trim()) ? parseInt(picksRaw.trim(), 10) : 1;
    if (isNaN(picks) || picks <= 0) return null;

    // Denominator = NoDrop + all ProbN values present in this row.
    let total = 0;
    for (let i = 1; ; i++) {
        if (!getColumn("treasureclassex", "Prob" + i)) break;
        const v = ctx.row["Prob" + i];
        if (v && v.trim()) {
            const p = parseInt(v.trim(), 10);
            if (!isNaN(p)) total += p;
        }
    }
    const noDropRaw = ctx.row["NoDrop"];
    if (noDropRaw && noDropRaw.trim()) {
        const nd = parseInt(noDropRaw.trim(), 10);
        if (!isNaN(nd)) total += nd;
    }

    if (total <= 0) return null;

    const perRoll = prob / total;
    const atLeastOnce = 1 - Math.pow(1 - perRoll, picks);

    const fmt = (x: number): string => {
        // Up to 4 decimal places, trailing zeros stripped.
        let s = (x * 100).toFixed(4);
        s = s.replace(/\.?0+$/, "");
        return s + "%";
    };

    const idx = probMatch[1];
    const itemValue = (ctx.row["Item" + idx] || "").trim();

    const parts: string[] = [];
    if (itemValue) {
        parts.push("**" + itemValue + "**");
        parts.push("");
    }

    if (picks > 1) {
        parts.push("Per-roll chance: " + fmt(perRoll) + " (" + prob + " / " + total + ")");
        parts.push("At least once in " + picks + " picks: **" + fmt(atLeastOnce) + "**");
    } else {
        parts.push("Chance: **" + fmt(perRoll) + "** (" + prob + " / " + total + ")");
    }

    return { content: parts.join("\n") };
}
