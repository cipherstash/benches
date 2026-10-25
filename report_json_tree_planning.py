"""Planning cost of each format's subtree read body (report/JSON_TREE_MERGED.md,
section 5): the function body as a standalone statement, its arguments as
parameters, prepared afresh under a generic plan each time (as the SQL-function
executor plans it), EXPLAIN (ANALYZE, SUMMARY) of one execution; the median
Planning Time and Execution Time of the last N - 5 of N, for three reads of the
plan files. Usage: report_json_tree_planning.py <rows>, from the repo root."""
import json, re, statistics, subprocess, sys

OUT = "results/query/merged"
DBS = {"gates": "jsonbench_base", "derivable": "jsonbench_new", "merged": "jsonbench_merged"}
N = 50


def psql(db, sql):
    r = subprocess.run(["psql", "-h", "localhost", "-p", "5400", "-U", "postgres", "-d", db, "-tAX", "-v", "ON_ERROR_STOP=1"],
                       input=sql, capture_output=True, text=True)
    if r.returncode:
        sys.exit("psql failed: " + r.stderr[:600])
    return r.stdout


def body(db, fmt):
    fn = "json_tree_subtree_at" if fmt == "gates" else "json_tree_subtree"
    src = psql(db, f"select prosrc from pg_proc where proname='{fn}'")
    src = re.sub(r"--[^\n]*", "", src)
    if fmt == "gates":
        args = ["e", "token", "gates", "budget"]
    else:
        args = ["e", "tokens", "budget"]
    for i, a in enumerate(args, 1):
        src = re.sub(rf"(?<![\w.]){a}(?![\w])", f"${i}", src)
    return src


def lit(v):
    if isinstance(v, list):
        return "ARRAY[" + ",".join("'" + x + "'" for x in v) + "]::text[]"
    return "'" + str(v) + "'"


def main():
    rows = sys.argv[1] if len(sys.argv) > 1 else "10000"
    res = {}
    for fmt, db in DBS.items():
        plan = json.load(open(f"{OUT}/plan_{fmt}_{rows}.json"))
        b = body(db, fmt)
        for name in ["read/first_name", "read/company", "read/orders[2].status"]:
            s = next(x for x in plan["scenarios"] if x["name"] == name)
            rep = plan["representative"][s["table"]]
            args = [a.get("Text") or a.get("TextArray") for a in s["args"]]
            budget = 64
            typ = "public.eql_v4_json_tree"
            if fmt == "gates":
                params = f":'v', {lit(args[0])}::text, {lit(args[1])}, {budget}"
                types = f"{typ}, text, text[], integer"
            else:
                params = f":'v', {lit(args[0])}, {budget}"
                types = f"{typ}, text[], integer"
            one = (f"PREPARE q({types}) AS {b};\n"
                   f"EXPLAIN (ANALYZE, SUMMARY, TIMING OFF) EXECUTE q({params});\nDEALLOCATE q;\n")
            sql = (f"SELECT value::text AS v FROM {s['table']} WHERE id = {rep} \\gset\n"
                   "SET plan_cache_mode = force_generic_plan;\n" + one * N)
            out = psql(db, sql)
            plan_t = [float(m) for m in re.findall(r"Planning Time: ([\d.]+)", out)]
            exec_t = [float(m) for m in re.findall(r"Execution Time: ([\d.]+)", out)]
            res[(fmt, name)] = (statistics.median(plan_t[5:]), statistics.median(exec_t[5:]))
            print(f"{fmt:9} {name:24} planning {res[(fmt, name)][0]:.3f} ms  execution {res[(fmt, name)][1]:.3f} ms", flush=True)


main()
