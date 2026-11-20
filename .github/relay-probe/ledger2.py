"""probe2.py <bin> <workdir>: fj#240, the 12-file literal cap, fj#252, fj#253 as two-commit repos; prints each answer."""
import json, subprocess, sys, pathlib, shutil
BIN, WORK = sys.argv[1], pathlib.Path(sys.argv[2])
F0 = "def compute_total(xs):\n    return sum(xs)\n"
F1 = "def compute_total(xs):\n    return sum(xs) + 0\n"
INIT = lambda b: f"class Plain:\n    def __init__(self):\n        self.v = {b}\n"
META = lambda b: f"class Meta(type):\n    def create(cls):\n        return cls()\n\n\nclass Model(metaclass=Meta):\n    def __init__(self):\n        self.v = {b}\n"
SHAPES = {
  "fj240_paths_narrowed": (["src"], {"src/app/core.py": F0, "src/app/use.py": "from app.core import compute_total\n\n\ndef g(xs):\n    return compute_total(xs)\n", "tests/test_use.py": "from app.use import g\n\n\ndef test_g():\n    assert g([1]) == 1\n"}, {"src/app/core.py": F1}),
  "cap12_literal_namers": ([], {"app/calc.py": F0, **{f"conf/h{i:02}.json": '{"handler": "compute_total"}\n' for i in range(15)}}, {"app/calc.py": F1}),
  "fj252_type_of_instance": ([], {"m/plain.py": INIT(1), "m/use.py": "def clone(obj):\n    return type(obj)()\n"}, {"m/plain.py": INIT(2)}),
  "fj253_metaclass_method": ([], {"m/model.py": META(1), "m/use.py": "from m.model import Model\n\n\ndef plain():\n    return Model()\n\n\ndef build():\n    return Model.create()\n"}, {"m/model.py": META(2)}),
  "fj252_issue": ([], {"main.py": "class Base:\n    def __init__(self):\n        pass\n\n\nclass Child(Base):\n    def __init__(self):\n        {b}\n\n\ndef duplicate(obj):\n    return type(obj)()\n\n\nduplicate(Child())\n".format(b="pass")}, {"main.py": "class Base:\n    def __init__(self):\n        pass\n\n\nclass Child(Base):\n    def __init__(self):\n        {b}\n\n\ndef duplicate(obj):\n    return type(obj)()\n\n\nduplicate(Child())\n".format(b="self.x = 1")}),
}
only = sys.argv[3:]
def git(d, *a): subprocess.run(["git", "-C", str(d), *a], check=True, capture_output=True)
for name, (paths, base, change) in SHAPES.items():
    if only and name not in only: continue
    d = WORK / name
    shutil.rmtree(d, ignore_errors=True); d.mkdir(parents=True)
    git(d, "init", "-q", "-b", "main")
    for step, files in (("base", base), ("change", change)):
        for p, t in files.items():
            f = d / p; f.parent.mkdir(parents=True, exist_ok=True); f.write_text(t)
        git(d, "add", "-A"); git(d, "-c", "user.name=p", "-c", "user.email=p@p", "-c", "commit.gpgsign=false", "commit", "-q", "-m", step)
    env = {"PATH": "/usr/bin:/bin", "HOME": str(WORK), "DIFFCTX_TOKEN_CACHE_DIR": str(WORK / ".cache")}
    args = [BIN, *([str(d / p) for p in paths] or [str(d)]), "--diff", "HEAD~1..HEAD", "--mode", "impact"]
    md = subprocess.run(args + ["-f", "md", "-q"], capture_output=True, text=True, env=env, cwd=d).stdout
    js = json.loads(subprocess.run(args + ["--format", "json", "--quiet"], capture_output=True, text=True, env=env, cwd=d).stdout)
    print(f"--- {name}: callers={js.get('summary', {}).get('callers', '(1.20.0: no summary)')} tests={js.get('tests', {}).get('definitions_without_link')}")
    print(md.strip())
