//! Compiler plugin for Rust-Phf
//!
//! See the documentation for the `phf` crate for more details.
#[crate_id="github.com/sfackler/rust-phf/phf_mac"];
#[crate_type="lib"];
#[doc(html_root_url="http://www.rust-ci.org/sfackler/rust-phf/doc")];
#[feature(managed_boxes, macro_registrar)];

extern mod syntax;
extern mod phf;

use std::hashmap::HashMap;
use std::rand;
use std::uint;
use std::vec;
use syntax::ast;
use syntax::ast::{Name, TokenTree, LitStr, MutImmutable, Expr, ExprVec, ExprLit};
use syntax::codemap::Span;
use syntax::ext::base::{SyntaxExtension,
                        ExtCtxt,
                        MacResult,
                        MRExpr,
                        NormalTT,
                        SyntaxExpanderTT,
                        SyntaxExpanderTTExpanderWithoutContext};
use syntax::parse;
use syntax::parse::token;
use syntax::parse::token::{COMMA, EOF, FAT_ARROW};

use phf::Keys;

static DEFAULT_LAMBDA: uint = 5;

#[macro_registrar]
#[doc(hidden)]
pub fn macro_registrar(register: |Name, SyntaxExtension|) {
    register(token::intern("phf_map"),
             NormalTT(~SyntaxExpanderTT {
                expander: SyntaxExpanderTTExpanderWithoutContext(expand_mphf_map),
                span: None
             },
             None));
}

struct Entry {
    key_str: @str,
    key: @Expr,
    value: @Expr
}

fn expand_mphf_map(cx: &mut ExtCtxt, sp: Span, tts: &[TokenTree]) -> MacResult {
    let mut parser = parse::new_parser_from_tts(cx.parse_sess(), cx.cfg(),
                                                tts.to_owned());
    let mut entries = ~[];

    while parser.token != EOF {
        let key = parser.parse_expr();

        let key_str = match key.node {
            ExprLit(lit) => {
                match lit.node {
                    LitStr(s, _) => s,
                    _ => cx.span_fatal(key.span, "expected string literal"),
                }
            }
            _ => cx.span_fatal(key.span, "expected string literal"),
        };

        if !parser.eat(&FAT_ARROW) {
            cx.span_fatal(parser.span, "expected `=>`");
        }

        let value = parser.parse_expr();

        entries.push(Entry {
            key_str: key_str,
            key: key,
            value: value
        });

        if !parser.eat(&COMMA) && parser.token != EOF {
            cx.span_fatal(parser.span, "expected `,`");
        }
    }

    entries.sort_by(|a, b| a.key_str.cmp(&b.key_str));
    check_for_duplicates(cx, sp, entries);
    let state;
    loop {
        match generate_hash(entries) {
            Some(s) => {
                state = s;
                break;
            }
            None => {}
        }
    }

    let len = entries.len();
    let len = quote_expr!(&*cx, $len);

    let k1 = state.keys.k1;
    let k2_g = state.keys.k2_g;
    let k2_f1 = state.keys.k2_f1;
    let k2_f2 = state.keys.k2_f2;
    let keys = quote_expr!(&*cx, phf::Keys {
        k1: $k1,
        k2_g: $k2_g,
        k2_f1: $k2_f1,
        k2_f2: $k2_f2,
    });
    let disps = state.disps.iter().map(|&(d1, d2)| {
            quote_expr!(&*cx, ($d1, $d2))
        }).collect();
    let disps = @Expr {
        id: ast::DUMMY_NODE_ID,
        node: ExprVec(disps, MutImmutable),
        span: sp,
    };
    let entries = state.map.iter().map(|&idx| {
            match idx {
                Some(idx) => {
                    let Entry { key, value, .. } = entries[idx];
                    quote_expr!(&*cx, Some(($key, $value)))
                }
                None => quote_expr!(&*cx, None),
            }
        }).collect();
    let entries = @Expr {
        id: ast::DUMMY_NODE_ID,
        node: ExprVec(entries, MutImmutable),
        span: sp,
    };

    MRExpr(quote_expr!(cx, phf::PhfMap {
        len: $len,
        keys: $keys,
        disps: &'static $disps,
        entries: &'static $entries,
    }))
}

fn check_for_duplicates(cx: &mut ExtCtxt, sp: Span, entries: &[Entry]) {
    let mut in_dup = false;
    for window in entries.windows(2) {
        let ref a = window[0];
        let ref b = window[1];
        if a.key_str == b.key_str {
            if !in_dup {
                cx.span_err(sp, format!("duplicate key \"{}\"", a.key_str));
                cx.span_err(a.key.span, "one occurrence here");
                in_dup = true;
            }
            cx.span_err(b.key.span, "one occurrence here");
        } else {
            in_dup = false;
        }
    }
    cx.parse_sess().span_diagnostic.handler().abort_if_errors();
}

struct HashState {
    keys: Keys,
    disps: ~[(uint, uint)],
    map: ~[Option<uint>],
}

fn generate_hash(entries: &[Entry]) -> Option<HashState> {
    struct Bucket {
        idx: uint,
        keys: ~[uint],
    }

    let keys = Keys {
        k1: rand::random(),
        k2_g: rand::random(),
        k2_f1: rand::random(),
        k2_f2: rand::random(),
    };

    if entries.is_empty() {
        return Some(HashState {
            keys: keys,
            disps: ~[],
            map: ~[],
        })
    }

    let buckets_len = (entries.len() + DEFAULT_LAMBDA - 1) / DEFAULT_LAMBDA;
    let mut buckets = vec::from_fn(buckets_len,
                                   |i| Bucket { idx: i, keys: ~[] });

    for (i, entry) in entries.iter().enumerate() {
        let idx = keys.hash1(entry.key_str.as_slice()) % buckets_len;
        buckets[idx].keys.push(i);
    }

    // Sort descending
    buckets.sort_by(|a, b| b.keys.len().cmp(&a.keys.len()));

    let table_len = entries.len();
    let mut map = vec::from_elem(table_len, None);
    let mut disps = vec::from_elem(buckets_len, None);
    let mut try_map = HashMap::new();
    'buckets: for bucket in buckets.iter() {
        for d1 in range(0, table_len) {
            'disps: for d2 in range(0, table_len) {
                try_map.clear();
                for &key in bucket.keys.iter() {
                    let idx = keys.hash2(entries[key].key_str.as_slice(), d1, d2) % table_len;
                    if try_map.find(&idx).is_some() || map[idx].is_some() {
                        continue 'disps;
                    }
                    try_map.insert(idx, key);
                }

                // We've picked a good set of disps
                disps[bucket.idx] = Some((d1, d2));
                for (&idx, &key) in try_map.iter() {
                    map[idx] = Some(key);
                }
                continue 'buckets;
            }
        }

        // Unable to find displacements for a bucket
        return None;
    }

    let disps = disps.move_iter().map(|i| i.expect("should have a bucket")).collect();

    Some(HashState {
        keys: keys,
        disps: disps,
        map: map,
    })
}