/**
 * Type declarations for vector-lsp plugins.
 *
 * Reference this file in your plugin for IDE type checking:
 *
 *   // @ts-check
 *   /// <reference path="./vector-lsp-plugin.d.ts" />
 *
 * Plugin files define one or both of the global functions below.
 * The server auto-registers them at startup; no import/export needed.
 *
 * NOTE: Inline type annotations on function parameters (e.g. `x: string`)
 * require pre-compiling your .ts file to .js.  Structural declarations
 * (interface / type aliases) are stripped at load time and are safe to use.
 */

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

interface WorkspaceFile {
    headers: string[];
    rows: WorkspaceRow[];
}

/** A data row with positional metadata included. */
interface WorkspaceRow {
    [column: string]: string | number | Record<string, number>;
    /** 0-based source line number for this row. */
    __line: number;
    /** Character offset (UTF-16) of each column's value start within its line. */
    __colstarts: Record<string, number>;
}

// ---------------------------------------------------------------------------
// validate(ctx) — custom diagnostic checks
// ---------------------------------------------------------------------------

interface PluginContext {
    /** Stem of the file being validated (e.g. `"cubemain"` for `cubemain.txt`). */
    file: string;
    /** Ordered column names for the current file. */
    headers: string[];
    /** Data rows for the current file, each with `__line` and `__colstarts`. */
    rows: WorkspaceRow[];
    /**
     * All files in the workspace keyed by lowercase stem.
     * Example: `ctx.workspace["monstats"]`
     */
    workspace: Record<string, WorkspaceFile>;
}

interface PluginDiagnostic {
    /** 0-based line number. */
    line: number;
    /** 0-based start character (UTF-16 offset). Use `row.__colstarts[col]`. */
    col: number;
    /** 0-based end character (exclusive). Defaults to `col` when omitted. */
    endCol?: number;
    /** Defaults to `"warning"`. */
    severity?: "error" | "warning" | "info" | "hint";
    message: string;
}

/**
 * Define this function to add custom diagnostic checks.
 * Return an empty array for files this plugin does not handle.
 *
 * @example
 * function validate(ctx) {
 *   if (ctx.file !== "cubemain") return [];
 *   var diags = [];
 *   var nIdx = ctx.headers.indexOf("numinputs");
 *   ctx.rows.forEach(function(row) {
 *     var n = parseInt(row["numinputs"] || "0", 10);
 *     for (var i = 1; i <= 7; i++) {
 *       var filled = (row["input " + i] || "") !== "";
 *       if (i <= n && !filled) {
 *         diags.push({
 *           line: row.__line,
 *           col: row.__colstarts["input " + i] || 0,
 *           severity: "warning",
 *           message: "input " + i + " is required when numinputs=" + n,
 *         });
 *       }
 *     }
 *   });
 *   return diags;
 * }
 */
declare function validate(ctx: PluginContext): PluginDiagnostic[];

// ---------------------------------------------------------------------------
// hover(ctx) — custom hover content
// ---------------------------------------------------------------------------

interface HoverContext {
    /** Stem of the file containing the hovered cell. */
    file: string;
    /** Name of the column being hovered. */
    col: string;
    /** Raw string value of the hovered cell. */
    value: string;
    /** 0-based line number of the row containing the hovered cell. */
    rowLine: number;
    /** All cell values in the hovered row, keyed by column name. */
    row: Record<string, string>;
    /** All files in the workspace (same shape as PluginContext.workspace). */
    workspace: Record<string, WorkspaceFile>;
}

interface HoverResult {
    /** Markdown content appended after the schema description (if any). */
    content: string;
}

/**
 * Define this function to supply custom hover content.
 * Return `null` for columns this plugin does not handle.
 * The returned content is appended after the schema description with a divider.
 *
 * @example
 * function hover(ctx) {
 *   if (ctx.file !== "cubemain" || ctx.col !== "op") return null;
 *   var ops = { "1": "Add value", "2": "Multiply", "3": "Set value" };
 *   var label = ops[ctx.value];
 *   if (!label) return null;
 *   return { content: "Op **" + ctx.value + "**: " + label };
 * }
 */
declare function hover(ctx: HoverContext): HoverResult | null;
