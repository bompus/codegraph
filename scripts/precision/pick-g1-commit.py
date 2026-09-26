"""Check out, for one G1 repository, the commit (weekly, May-Aug 2026) at which
the most graded rows' source text sits on its recorded line. repowise-bench
records no pins for these repositories. Usage: python3 pick-g1-commit.py zod"""
import json,os,subprocess,sys,datetime
base="repowise-bench/graph/experiments/g1-edge-precision/rows"
def rows_for(repo):
    out=[]
    for lang in ["typescript","python","csharp"]:
        d=json.load(open(f"{base}/{lang}-codegraph.json"))
        out+=[r for r in d["rows"] if r["repo"]==repo and r.get("source_line")]
    return out
def score(repo,rows):
    ok=0
    for r in rows:
        p=os.path.join("repos",repo,r["file"])
        if not os.path.exists(p): continue
        lines=open(p,encoding="utf-8",errors="replace").read().split("\n")
        frag=r["source_line"].split("`")[0].strip()[:25]
        if r["line"]<=len(lines) and frag in lines[r["line"]-1]: ok+=1
    return ok
repo=sys.argv[1]; rows=rows_for(repo); g=["git","-C",f"repos/{repo}"]
best=None
d=datetime.date(2026,5,1)
while d<=datetime.date(2026,8,12):
    c=subprocess.run(g+["rev-list","-1",f"--before={d}","HEAD@{0}" if False else "origin/HEAD"],capture_output=True,text=True).stdout.strip()
    if c:
        subprocess.run(g+["-c","advice.detachedHead=false","checkout","-q",c],check=True)
        s=score(repo,rows)
        if best is None or s>best[0]: best=(s,c,str(d))
        print(repo,d,c[:8],s,"/",len(rows),flush=True)
    d+=datetime.timedelta(days=7)
subprocess.run(g+["-c","advice.detachedHead=false","checkout","-q",best[1]],check=True)
print("BEST",repo,best)
