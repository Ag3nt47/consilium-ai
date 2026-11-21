"""Print a bounded, offline outline of a Python source file."""
import argparse
import ast
import os
import stat
import sys
from pathlib import Path

SOURCE_LIMIT = 1024 * 1024
ROW_LIMIT = 100


def read_source(path):
    fd = os.open(path, os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_BINARY", 0))
    with os.fdopen(fd, "rb") as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > SOURCE_LIMIT:
            raise ValueError("Use a regular Python source file up to 1 MiB.")
        text = source.read(SOURCE_LIMIT + 1)
        if len(text) > SOURCE_LIMIT:
            raise ValueError("The source exceeds 1 MiB.")
        return text


def declarations(nodes, prefix=""):
    for node in nodes:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            yield node, "function", prefix + node.name
        elif isinstance(node, ast.ClassDef):
            yield node, "class", prefix + node.name
            yield from declarations(node.body, prefix + node.name + ".")
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            module = "." * node.level + (node.module or "") if isinstance(node, ast.ImportFrom) else ""
            for alias in node.names:
                name = f"{module} {alias.name}" if module else alias.name
                yield node, "import", name
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for target in targets:
                if isinstance(target, ast.Name):
                    yield node, "binding", prefix + target.id


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file", type=Path)
    parser.add_argument("--filter", default="", help="Show symbols containing this text.")
    args = parser.parse_args()
    try:
        source = ast.parse(read_source(args.file), filename=str(args.file))
    except (OSError, SyntaxError, ValueError, RecursionError) as error:
        parser.exit(1, f"{error}\n")

    match = args.filter.casefold()
    total = 0
    for node, kind, name in declarations(source.body):
        if match not in name.casefold():
            continue
        total += 1
        if total <= ROW_LIMIT:
            lines = str(node.lineno) if node.lineno == node.end_lineno else f"{node.lineno}-{node.end_lineno}"
            print(f"{lines} {kind} {name[:140]}")
    if total > ROW_LIMIT:
        print(f"{total - ROW_LIMIT} additional rows omitted.")


if __name__ == "__main__":
    sys.exit(main())
