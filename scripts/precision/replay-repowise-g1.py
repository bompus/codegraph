"""Replay repowise-bench G1 CodeGraph-1.5.0 graded rows against a fork index.

Run from a scratch directory holding `repowise-bench/` (a clone of
github.com/repowise-dev/repowise-bench) and `repos/<repo>/`, each checked out
(pick-g1-commit.py) and indexed with `codegraph init -y`. Usage:
    python3 replay-repowise-g1.py typescript python csharp kotlin
Per row: locate the call site (exact line, else the nearest line within +-40
holding the source fragment, or the callee name when a row has no source text),
then report what the fork binds there for the same callee name: `same` target,
`changed` target (grade by hand), `dropped` (left unresolved) or `absent`.
Writes replay-<langs>.json beside the counts it prints."""
import json,os,re,sqlite3,sys,collections
base="repowise-bench/graph/experiments/g1-edge-precision/rows"
def frag_of(src):
    s=src.split("`")[0].strip().strip(".").strip()
    s=re.sub(r"^\.\.\.\$\{","",s); s=s.replace("}...","")
    return s[:25]
def locate(lines,line,frag):
    if not frag: return line
    if 0<line<=len(lines) and frag in lines[line-1]: return line
    hits=[i+1 for i,l in enumerate(lines) if frag in l and abs(i+1-line)<=40]
    return min(hits,key=lambda h:abs(h-line)) if hits else None
PY_FMT=re.compile(r"^(\S+?):(\d+)\s*(.*)$")
def tname_of(target):
    m=PY_FMT.match(target)
    head=(m.group(3) if m else target).split(" (")[0].strip() or "?"
    return re.split(r"::|\.",head)[-1]
def hint_of(target):
    m=PY_FMT.match(target)
    if m: return (m.group(1),int(m.group(2)))
    m=re.search(r"\(([^:,()]+):(\d+)",target)
    return (m.group(1),int(m.group(2))) if m else (None,None)
def norm(q):
    q=q.replace(".","::")
    return [x for x in q.split("::") if x]
def same_target(target, t):
    m0=PY_FMT.match(target)
    head=(m0.group(3) if m0 else target).split(" (")[0].strip()
    parts=head.split("::")
    if parts and ("/" in parts[0] or re.search(r"\.(cs|py|ts|kt|java)$",parts[0])):
        path=parts[0]; parts=parts[1:]
        if not t[2].endswith(path.lstrip("./")): return False
    hp,hl=hint_of(target)
    if hp is not None and not t[2].endswith(hp): return False
    if hl is not None and t[3]!=hl: return False
    m=re.search(r"target_line (\d+)",target)
    if m and t[3]!=int(m.group(1)): return False
    g=norm("::".join(parts)); f=norm(t[1])
    k=min(len(g),2)
    return f[-k:]==g[-k:] if g else True
out=collections.Counter(); detail=[]
langs=sys.argv[1:]
for lang in langs:
    d=json.load(open(f"{base}/{lang}-codegraph.json"))
    for r in d["rows"]:
        repo=r["repo"]; p=os.path.join("repos",repo,r["file"])
        if not os.path.exists(p): out[(lang,r["verdict"],"nofile")]+=1; continue
        lines=open(p,encoding="utf-8",errors="replace").read().split("\n")
        L=locate(lines,r["line"],frag_of(r["source_line"]) if r.get("source_line") else tname_of(r["target"]))
        if L is None: out[(lang,r["verdict"],"unlocated")]+=1; detail.append((lang,r["verdict"],"unlocated",r["file"],r["line"],r["target"],"")); continue
        db=sqlite3.connect(f"repos/{repo}/.codegraph/codegraph.db")
        tn=tname_of(r["target"]); hp,hl=hint_of(r["target"])
        rows=db.execute("""select t.name,t.qualified_name,t.file_path,t.start_line,t.kind,e.kind,e.metadata from edges e join nodes s on e.source=s.id join nodes t on e.target=t.id
             where s.file_path=? and e.line=? and e.kind in ('calls','instantiates','references') and t.name=?""",(r["file"],L,tn)).fetchall()
        if rows:
            t=rows[0]
            same = same_target(r["target"], t)
            status="same" if same else "changed"
            out[(lang,r["verdict"],status)]+=1
            detail.append((lang,r["verdict"],status,r["file"],L,r["target"],f"{t[1]} ({t[2]}:{t[3]}, {t[4]}) via {json.loads(t[6] or '{}').get('resolvedBy')}"))
        else:
            u=db.execute("select reference_name,status,failure_reason from unresolved_refs where file_path=? and line=? and (reference_name=? or reference_name like ?)",(r["file"],L,tn,'%.'+tn)).fetchall()
            status="dropped" if u else "absent"
            out[(lang,r["verdict"],status)]+=1
            detail.append((lang,r["verdict"],status,r["file"],L,r["target"],str(u[:1])))
for k in sorted(out): print(k,out[k])
json.dump(detail,open(f"replay-{'-'.join(langs)}.json","w"),indent=1)
