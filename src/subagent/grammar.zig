//! grammar.zig — GBNF grammar constraints for LLM structured output.
//!
//! Defines grammar strings used with Ollama's grammar-constrained generation
//! to force LLM batch classification output into valid JSON.

pub const classification_grammar =
    \\root ::= "[" space (classification ("," space classification)*)? "]" space
    \\classification ::= "{" space "\"index\"" space ":" space integer "," space "\"action\"" space ":" space action_type ("," space "\"params\"" space ":" space params_obj)? space "}"
    \\action_type ::= "\"bash\"" | "\"read\"" | "\"explain\"" | "\"edit\"" | "\"diary\"" | "\"checklist\""
    \\params_obj ::= "\"" space key space "\":\"" space value space "\"" ("," space "\"" space key space "\":\"" space value space "\"")*
    \\key ::= "command" | "path" | "query" | "content" | "item_index"
    \\value ::= [^"\\]*
    \\string ::= "\"" ([^"\\] | "\\" [\\"/bfnrt])* "\""
    \\integer ::= [0-9]+
    \\space ::= [ \t\n\r]*
;

pub const route_grammar =
    \\root ::= "{" space "\"action\"" space ":" space action_type "," space "\"params\"" space ":" space params_obj space "}"
    \\action_type ::= "\"bash\"" | "\"read\"" | "\"explain\"" | "\"edit\"" | "\"diary\"" | "\"checklist\""
    \\params_obj ::= "{" space (pair ("," space pair)*)? space "}"
    \\pair ::= "\"" space key space "\"" space ":" space (string | integer)
    \\key ::= "command" | "path" | "query" | "content" | "item_index" | "line_start" | "line_end"
    \\string ::= "\"" ([^"\\] | "\\" [\\"/bfnrt])* "\""
    \\integer ::= [0-9]+
    \\space ::= [ \t\n\r]*
;

pub const synth_grammar =
    \\root ::= "{" space "\"summary\"" space ":" space string ("," space "\"followup\"" space ":" space action_type)? space "}"
    \\action_type ::= "\"bash\"" | "\"read\"" | "\"explain\"" | "\"edit\"" | "\"diary\"" | "\"checklist\"" | "\"unknown\""
    \\string ::= "\"" ([^"\\] | "\\" [\\"/bfnrt])* "\""
    \\space ::= [ \t\n\r]*
;
