"""probe.py <bin> <workdir>: build each ledger shape as a two-commit repo, run impact on HEAD~1..HEAD, report the claim."""
import json, subprocess, sys, pathlib, shutil
BIN, WORK = sys.argv[1], pathlib.Path(sys.argv[2])
SH0 = 'slugify() {\n  echo "$1" | tr A-Z a-z\n}\n'
SH1 = 'slugify() {\n  echo "$1" | tr A-Z a-z | tr " " -\n}\n'
J = "src/main/java/org/example/"
def jfile(pkg, body): return f"package {pkg};\n\n{body}\n"
LISTENER = lambda ann, imp, b: jfile("org.example.app", f"{imp}\npublic class Listener {{\n    {ann}\n    public void onOrder(Object e) {{\n        System.out.println({b});\n    }}\n}}")
CTRL = lambda ret, imp: jfile("org.example.app", f"""import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;
import org.springframework.http.ResponseEntity;
{imp}
@RestController
public class UserController {{
    @GetMapping("/users")
    public {ret} list() {{
        return null;
    }}
}}""")
DTO = lambda b: jfile("org.example.app", f"public class UserDto {{\n    private String name;\n    public String getName() {{\n        return {b};\n    }}\n}}")
BASE = lambda b: jfile("org.example.app", f"public class BaseDto {{\n    private long id;\n    public long getId() {{\n        return {b};\n    }}\n}}")
SHAPES = {
  "bbshq_ruby_heredoc": ({"lib/strings.sh": SH0, "bin/run.rb": 'system(<<~SH)\n  set -e\n  slugify abc\nSH\n'}, {"lib/strings.sh": SH1}),
  "bbshq_js_template": ({"lib/strings.sh": SH0, "run.js": 'const { execSync } = require("child_process");\nexecSync(`\n  slugify abc\n`);\n'}, {"lib/strings.sh": SH1}),
  "bbshq_py_triple": ({"lib/strings.sh": SH0, "run.py": 'import subprocess\nsubprocess.run("""\nslugify abc\n""", shell=True)\n'}, {"lib/strings.sh": SH1}),
  "bbshq_go_raw": ({"lib/strings.sh": SH0, "go.mod": "module ex\n\ngo 1.22\n", "main.go": 'package main\n\nimport "os/exec"\n\nfunc main() {\n\texec.Command("bash", "-c", `\nslugify abc\n`).Run()\n}\n'}, {"lib/strings.sh": SH1}),
  "bbjann_star_import": ({J+"app/Listener.java": LISTENER("@Subscribe", "import org.example.bus.*;", '"a"')}, {J+"app/Listener.java": LISTENER("@Subscribe", "import org.example.bus.*;", '"b"')}),
  "bbjann_qualified_ann": ({J+"app/Listener.java": LISTENER("@org.example.bus.Subscribe", "", '"a"')}, {J+"app/Listener.java": LISTENER("@org.example.bus.Subscribe", "", '"b"')}),
  "bbjann_page_payload": ({J+"app/UserController.java": CTRL("ResponseEntity<Page<UserDto>>", "import org.springframework.data.domain.Page;"), J+"app/UserDto.java": DTO("name")}, {J+"app/UserDto.java": DTO("name.trim()")}),
  "bbjann_inherited_getter": ({J+"app/UserController.java": CTRL("ResponseEntity<UserDto>", ""), J+"app/UserDto.java": jfile("org.example.app", "public class UserDto extends BaseDto {\n    public String getName() {\n        return \"n\";\n    }\n}"), J+"app/BaseDto.java": BASE("id")}, {J+"app/BaseDto.java": BASE("id + 0")}),
  "control_ruby_oneline": ({"lib/strings.sh": SH0, "bin/run.rb": 'system("slugify abc")\n'}, {"lib/strings.sh": SH1}),
  "control_explicit_import": ({J+"app/Listener.java": LISTENER("@Subscribe", "import org.example.bus.Subscribe;", '"a"')}, {J+"app/Listener.java": LISTENER("@Subscribe", "import org.example.bus.Subscribe;", '"b"')}),
  "control_list_payload": ({J+"app/UserController.java": CTRL("ResponseEntity<List<UserDto>>", "import java.util.List;"), J+"app/UserDto.java": DTO("name")}, {J+"app/UserDto.java": DTO("name.trim()")}),
}
def git(d, *a): subprocess.run(["git", "-C", str(d), *a], check=True, capture_output=True)
only = sys.argv[3:]
for name, (base, change) in SHAPES.items():
    if only and name not in only: continue
    d = WORK / name
    shutil.rmtree(d, ignore_errors=True); d.mkdir(parents=True)
    git(d, "init", "-q", "-b", "main")
    for step, files in (("base", base), ("change", change)):
        for p, t in files.items():
            f = d / p; f.parent.mkdir(parents=True, exist_ok=True); f.write_text(t)
        git(d, "add", "-A"); git(d, "-c", "user.name=p", "-c", "user.email=p@p", "-c", "commit.gpgsign=false", "commit", "-q", "-m", step)
    env = {"PATH": "/usr/bin:/bin", "HOME": str(WORK), "DIFFCTX_TOKEN_CACHE_DIR": str(WORK / ".cache")}
    md = subprocess.run([BIN, str(d), "--diff", "HEAD~1..HEAD", "--mode", "impact", "-f", "md"], capture_output=True, text=True, env=env).stdout
    js = subprocess.run([BIN, str(d), "--diff", "HEAD~1..HEAD", "--mode", "impact", "--format", "json", "--quiet"], capture_output=True, text=True, env=env).stdout
    (d.parent / f"{name}.md").write_text(md)
    absent = "nothing calls them" in md or "named nowhere" in md
    try:
        s = json.loads(js)["summary"]["callers"]
        verdict = "ASSERTS ABSENCE" if absent and s.get("exact") else "hedged/named"
    except (ValueError, KeyError, TypeError, AttributeError):
        # 1.20.0's JSON has no summary.callers and no exactness: its claim is the answer's own sentence.
        line = next((l.strip() for l in md.splitlines() if "caller" in l or "calls them" in l or "named nowhere" in l), "")
        s = f"md: {line[:70]!r}"
        verdict = "ASSERTS ABSENCE (text)" if absent or "no callers" in md.lower() else "hedged/named"
    print(f"{name:26} callers={s} {verdict}")
