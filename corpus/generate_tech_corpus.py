#!/usr/bin/env python3
"""Generate technical/programming vocabulary corpus for SmartKey.

Hardcoded vocabulary — no downloads needed. Covers:
- Programming keywords and concepts (~500 unigrams)
- Common technical bigrams (~200)
- Common technical trigrams (~50)

Usage:
    python3 generate_tech_corpus.py [-o OUTPUT]
"""

import argparse
import json
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# Technical unigrams — sorted roughly by frequency in code/docs.
# Frequencies are synthetic but follow Zipf-like distribution.
# ---------------------------------------------------------------------------

_TECH_UNIGRAMS = {
    # --- Top tier: universal programming terms (freq 5000-10000) ---
    "function": 10000, "return": 9500, "value": 9000, "type": 8800,
    "data": 8500, "error": 8200, "file": 8000, "name": 7800,
    "code": 7600, "string": 7400, "number": 7200, "list": 7000,
    "object": 6800, "class": 6600, "method": 6500, "variable": 6400,
    "array": 6200, "key": 6000, "index": 5800, "null": 5600,
    "true": 5500, "false": 5400, "import": 5200, "module": 5000,

    # --- High tier: core concepts (freq 3000-5000) ---
    "server": 4900, "client": 4800, "request": 4700, "response": 4600,
    "database": 4500, "query": 4400, "table": 4300, "config": 4200,
    "test": 4100, "debug": 4000, "log": 3900, "output": 3800,
    "input": 3700, "path": 3600, "token": 3500, "parameter": 3400,
    "argument": 3300, "buffer": 3200, "memory": 3100, "thread": 3000,
    "process": 2950, "callback": 2900, "promise": 2850, "async": 2800,
    "await": 2750, "event": 2700, "handler": 2650, "listener": 2600,
    "interface": 2550, "struct": 2500, "enum": 2450, "boolean": 2400,
    "integer": 2350, "float": 2300, "double": 2250, "char": 2200,
    "byte": 2150, "binary": 2100, "hex": 2050, "hash": 2000,

    # --- Mid tier: architecture & patterns (freq 1500-2000) ---
    "api": 1950, "endpoint": 1900, "route": 1850, "middleware": 1800,
    "controller": 1750, "service": 1700, "repository": 1650, "model": 1600,
    "view": 1550, "template": 1500, "component": 1480, "widget": 1460,
    "plugin": 1440, "package": 1420, "library": 1400, "framework": 1380,
    "runtime": 1360, "compiler": 1340, "parser": 1320, "lexer": 1300,
    "syntax": 1280, "ast": 1260, "node": 1240, "tree": 1220,
    "graph": 1200, "stack": 1180, "queue": 1160, "heap": 1140,
    "cache": 1120, "proxy": 1100, "socket": 1080, "stream": 1060,
    "pipe": 1040, "channel": 1020, "mutex": 1000, "lock": 980,
    "semaphore": 960, "atomic": 940, "volatile": 920, "static": 900,
    "dynamic": 880, "abstract": 860, "virtual": 840, "override": 820,
    "implement": 800, "extend": 780, "inherit": 760, "polymorphism": 740,
    "encapsulation": 720, "singleton": 700, "factory": 680, "observer": 660,
    "iterator": 640, "decorator": 620, "adapter": 600, "facade": 580,
    "strategy": 560, "command": 540, "state": 520, "builder": 500,

    # --- Language keywords (freq 800-3000) ---
    "const": 2800, "let": 2700, "var": 2600, "def": 2500,
    "if": 2400, "else": 2300, "elif": 2200, "switch": 2100,
    "case": 2050, "while": 2000, "for": 1950, "loop": 1900,
    "break": 1850, "continue": 1800, "try": 1750, "catch": 1700,
    "except": 1650, "finally": 1600, "throw": 1550, "raise": 1500,
    "yield": 1450, "lambda": 1400, "closure": 1350, "generic": 1300,
    "trait": 1250, "protocol": 1200, "mixin": 1150, "macro": 1100,
    "annotation": 1050, "attribute": 1000, "property": 950,
    "getter": 900, "setter": 880, "constructor": 860, "destructor": 840,
    "allocator": 820, "pointer": 800, "reference": 780, "borrow": 760,
    "ownership": 740, "lifetime": 720, "scope": 700, "namespace": 680,
    "public": 660, "private": 640, "protected": 620, "internal": 600,
    "extern": 580, "unsafe": 560, "inline": 540, "readonly": 520,
    "mutable": 500, "immutable": 480, "optional": 460, "nullable": 440,

    # --- Web & networking (freq 400-1500) ---
    "http": 1500, "https": 1480, "url": 1460, "uri": 1440,
    "json": 1420, "xml": 1400, "html": 1380, "css": 1360,
    "dom": 1340, "ajax": 1320, "fetch": 1300, "cors": 1280,
    "cookie": 1260, "session": 1240, "auth": 1220, "oauth": 1200,
    "jwt": 1180, "bearer": 1160, "header": 1140, "body": 1120,
    "payload": 1100, "webhook": 1080, "websocket": 1060, "tcp": 1040,
    "udp": 1020, "dns": 1000, "ssl": 980, "tls": 960,
    "certificate": 940, "encryption": 920, "decrypt": 900, "cipher": 880,
    "digest": 860, "signature": 840, "nonce": 820, "salt": 800,
    "bcrypt": 780, "argon": 760, "sha": 740, "md5": 720,
    "base64": 700, "utf8": 680, "unicode": 660, "encoding": 640,
    "serialization": 620, "deserialization": 600, "marshal": 580,
    "unmarshal": 560, "protobuf": 540, "grpc": 520, "graphql": 500,
    "rest": 480, "soap": 460, "rpc": 440, "microservice": 420,
    "container": 400, "docker": 380, "kubernetes": 360, "pod": 340,

    # --- Database (freq 300-1200) ---
    "sql": 1200, "nosql": 1180, "schema": 1160, "migration": 1140,
    "transaction": 1120, "commit": 1100, "rollback": 1080, "partition": 1060,
    "primary": 1040, "foreign": 1020, "constraint": 1000, "join": 980,
    "select": 960, "insert": 940, "update": 920, "delete": 900,
    "where": 880, "groupby": 860, "orderby": 840, "limit": 820,
    "offset": 800, "cursor": 780, "rowset": 760, "bulk": 740,
    "shard": 720, "replica": 700, "cluster": 680, "ttl": 660,
    "redis": 640, "postgres": 620, "mysql": 600, "sqlite": 580,
    "mongo": 560, "elasticsearch": 540, "kafka": 520, "rabbitmq": 500,

    # --- DevOps & tools (freq 200-800) ---
    "git": 800, "branch": 780, "merge": 760, "rebase": 740,
    "checkout": 720, "stash": 700, "diff": 680, "patch": 660,
    "deploy": 640, "release": 620, "pipeline": 600, "ci": 580,
    "cd": 560, "build": 540, "compile": 520, "lint": 500,
    "format": 480, "refactor": 460, "optimize": 440, "profile": 420,
    "benchmark": 400, "coverage": 380, "mock": 360, "stub": 340,
    "fixture": 320, "assertion": 300, "unittest": 280, "pytest": 260,
    "jest": 240, "mocha": 220, "cypress": 200,

    # --- Data science & ML (freq 200-800) ---
    "tensor": 800, "matrix": 780, "vector": 760, "scalar": 740,
    "gradient": 720, "epoch": 700, "batch": 680, "layer": 660,
    "neuron": 640, "activation": 620, "dropout": 600, "embedding": 580,
    "transformer": 560, "attention": 540, "encoder": 520, "decoder": 500,
    "tokenizer": 480, "dataset": 460, "dataloader": 440, "optimizer": 420,
    "scheduler": 400, "checkpoint": 380, "inference": 360, "training": 340,
    "validation": 320, "accuracy": 300, "precision": 280, "recall": 260,
    "metric": 240, "loss": 220, "backpropagation": 200,

    # --- System programming (freq 200-600) ---
    "kernel": 600, "syscall": 580, "interrupt": 560, "signal": 540,
    "fork": 520, "exec": 500, "spawn": 480, "daemon": 460,
    "cron": 440, "ipc": 420, "mmap": 400, "brk": 380,
    "epoll": 360, "poll": 340, "ioctl": 320, "futex": 300,
    "ptrace": 280, "strace": 260, "valgrind": 240, "gdb": 220,
    "lldb": 200,

    # --- Misc common tech words (freq 200-500) ---
    "regex": 500, "pattern": 480, "match": 460, "replace": 440,
    "split": 420, "trim": 400, "concat": 380, "slice": 360,
    "map": 340, "filter": 320, "reduce": 300, "sort": 280,
    "search": 260, "find": 240, "contains": 220, "exists": 200,
    "append": 500, "push": 480, "pop": 460, "shift": 440,
    "unshift": 420, "splice": 400, "reverse": 380, "flatten": 360,
    "unique": 340, "distinct": 320, "aggregate": 300, "transform": 280,
    "validate": 260, "sanitize": 240, "escape": 220, "encode": 200,
    "decode": 500, "parse": 480, "stringify": 460, "interpolate": 440,
    "render": 420, "mount": 400, "unmount": 380, "hydrate": 360,
    "serialize": 340, "clone": 320, "copy": 300, "move": 280,
    "swap": 260, "rotate": 240, "permute": 220, "traverse": 200,
    "algorithm": 500, "complexity": 480, "recursion": 460, "iteration": 440,
    "memoization": 420, "precompute": 400, "throttle": 380, "debounce": 360,
    "pagination": 340, "infinite": 320, "scroll": 300, "lazy": 280,
    "eager": 260, "defer": 240, "suspend": 220, "resume": 200,
}

# ---------------------------------------------------------------------------
# Technical bigrams.
# ---------------------------------------------------------------------------

_TECH_BIGRAMS = [
    # Programming concepts
    ("return", "value", 800), ("source", "code", 780), ("null", "pointer", 760),
    ("data", "type", 740), ("data", "structure", 720), ("error", "handling", 700),
    ("error", "message", 680), ("file", "system", 660), ("file", "path", 640),
    ("hash", "map", 620), ("hash", "table", 600), ("hash", "function", 580),
    ("linked", "list", 560), ("binary", "tree", 540), ("binary", "search", 520),
    ("search", "algorithm", 500), ("sort", "algorithm", 480),
    ("time", "complexity", 460), ("space", "complexity", 440),
    ("memory", "allocation", 420), ("memory", "leak", 400),
    ("stack", "overflow", 380), ("buffer", "overflow", 360),
    ("race", "condition", 340), ("dead", "lock", 320),
    ("design", "pattern", 300), ("code", "review", 280),
    ("unit", "test", 260), ("integration", "test", 240),
    ("test", "case", 220), ("test", "suite", 200),

    # Web & API
    ("api", "endpoint", 700), ("api", "key", 680), ("api", "request", 660),
    ("http", "request", 640), ("http", "response", 620), ("http", "status", 600),
    ("status", "code", 580), ("content", "type", 560),
    ("request", "body", 540), ("response", "body", 520),
    ("query", "string", 500), ("query", "parameter", 480),
    ("base", "url", 460), ("web", "server", 440),
    ("web", "socket", 420), ("web", "application", 400),
    ("rest", "api", 380), ("json", "object", 360),
    ("json", "array", 340), ("json", "schema", 320),
    ("access", "token", 300), ("refresh", "token", 280),
    ("access", "control", 260), ("cross", "origin", 240),

    # Database
    ("primary", "key", 600), ("foreign", "key", 580),
    ("database", "connection", 560), ("database", "schema", 540),
    ("database", "migration", 520), ("connection", "pool", 500),
    ("connection", "string", 480), ("data", "model", 460),
    ("data", "source", 440), ("table", "name", 420),
    ("column", "name", 400), ("row", "count", 380),
    ("inner", "join", 360), ("left", "join", 340),
    ("outer", "join", 320), ("group", "by", 300),
    ("order", "by", 280), ("insert", "into", 260),

    # DevOps
    ("version", "control", 500), ("pull", "request", 480),
    ("merge", "conflict", 460), ("continuous", "integration", 440),
    ("continuous", "deployment", 420), ("build", "system", 400),
    ("build", "tool", 380), ("package", "manager", 360),
    ("dependency", "injection", 340), ("environment", "variable", 320),
    ("config", "file", 300), ("log", "file", 280),
    ("log", "level", 260), ("error", "log", 240),
    ("stack", "trace", 220), ("debug", "mode", 200),

    # Types & OOP
    ("return", "type", 500), ("generic", "type", 480),
    ("abstract", "class", 460), ("base", "class", 440),
    ("derived", "class", 420), ("inner", "class", 400),
    ("static", "method", 380), ("class", "method", 360),
    ("instance", "method", 340), ("virtual", "function", 320),
    ("pure", "function", 300), ("higher", "order", 280),
    ("callback", "function", 260), ("anonymous", "function", 240),
    ("arrow", "function", 220), ("lambda", "expression", 200),

    # ML/AI
    ("machine", "learning", 500), ("deep", "learning", 480),
    ("neural", "network", 460), ("training", "data", 440),
    ("test", "data", 420), ("feature", "extraction", 400),
    ("model", "training", 380), ("loss", "function", 360),
    ("learning", "rate", 340), ("batch", "size", 320),
    ("attention", "mechanism", 300), ("natural", "language", 280),
    ("language", "model", 260), ("large", "language", 240),

    # Security
    ("public", "key", 400), ("private", "key", 380),
    ("secret", "key", 360), ("encryption", "key", 340),
    ("security", "token", 320), ("sql", "injection", 300),
    ("cross", "site", 280), ("input", "validation", 260),
    ("authentication", "token", 240), ("authorization", "header", 220),

    # Misc
    ("open", "source", 400), ("command", "line", 380),
    ("regular", "expression", 360), ("string", "literal", 340),
    ("null", "value", 320), ("default", "value", 300),
    ("key", "value", 280), ("boolean", "expression", 260),
    ("type", "annotation", 240), ("type", "inference", 220),
    ("type", "safety", 200), ("compile", "time", 500),
    ("run", "time", 480), ("execution", "time", 460),
    ("response", "time", 440), ("load", "time", 420),
]

# ---------------------------------------------------------------------------
# Technical trigrams.
# ---------------------------------------------------------------------------

_TECH_TRIGRAMS = [
    ("end", "of", "file", 500),
    ("out", "of", "memory", 480),
    ("out", "of", "bounds", 460),
    ("out", "of", "range", 440),
    ("null", "pointer", "exception", 420),
    ("stack", "overflow", "error", 400),
    ("command", "line", "interface", 380),
    ("application", "programming", "interface", 360),
    ("object", "oriented", "programming", 340),
    ("model", "view", "controller", 320),
    ("create", "read", "update", 300),
    ("continuous", "integration", "deployment", 280),
    ("test", "driven", "development", 260),
    ("domain", "driven", "design", 240),
    ("don", "t", "repeat", 220),
    ("single", "responsibility", "principle", 200),
    ("open", "closed", "principle", 500),
    ("dependency", "injection", "container", 480),
    ("primary", "key", "constraint", 460),
    ("foreign", "key", "constraint", 440),
    ("cross", "site", "scripting", 420),
    ("sql", "injection", "attack", 400),
    ("public", "key", "infrastructure", 380),
    ("transport", "layer", "security", 360),
    ("secure", "socket", "layer", 340),
    ("hypertext", "transfer", "protocol", 320),
    ("uniform", "resource", "locator", 300),
    ("javascript", "object", "notation", 280),
    ("version", "control", "system", 260),
    ("integrated", "development", "environment", 240),
    ("natural", "language", "processing", 220),
    ("large", "language", "model", 200),
    ("machine", "learning", "model", 500),
    ("neural", "network", "architecture", 480),
    ("deep", "learning", "framework", 460),
    ("convolutional", "neural", "network", 440),
    ("recurrent", "neural", "network", 420),
    ("generative", "adversarial", "network", 400),
    ("long", "short", "term", 380),
    ("attention", "is", "all", 360),
    ("not", "a", "number", 340),
    ("end", "to", "end", 320),
    ("key", "value", "pair", 300),
    ("key", "value", "store", 280),
    ("hash", "map", "implementation", 260),
    ("binary", "search", "tree", 240),
    ("breadth", "first", "search", 220),
    ("depth", "first", "search", 200),
    ("linked", "list", "node", 500),
    ("doubly", "linked", "list", 480),
]


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(
        description="Generate technical/programming vocabulary corpus."
    )
    parser.add_argument(
        "-o",
        "--output",
        default=None,
        help="Output path (default: ~/.config/smartkey/corpus_tech.json).",
    )
    args = parser.parse_args(argv)

    output = Path(args.output) if args.output else (
        Path.home() / ".config" / "smartkey" / "corpus_tech.json"
    )

    corpus = {
        "unigrams": _TECH_UNIGRAMS,
        "bigrams": [
            {"ctx": ctx, "word": word, "count": count}
            for ctx, word, count in _TECH_BIGRAMS
        ],
        "trigrams": [
            {"w1": w1, "w2": w2, "word": w3, "count": count}
            for w1, w2, w3, count in _TECH_TRIGRAMS
        ],
    }

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(corpus, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    size_kb = output.stat().st_size / 1024
    print(
        f"[done] Wrote {output} ({size_kb:.0f} KB)\n"
        f"  Unigrams: {len(corpus['unigrams']):,}\n"
        f"  Bigrams:  {len(corpus['bigrams']):,}\n"
        f"  Trigrams: {len(corpus['trigrams']):,}",
    )


if __name__ == "__main__":
    main()
