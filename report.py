"""Aggregate scanned rows into the structures the CLI and dashboard display."""
from datetime import date, datetime, timedelta

import pricing


def _fill_days(days):
    if not days:
        return []
    start, end = date.fromisoformat(min(days)), date.fromisoformat(max(days))
    return [(start + timedelta(n)).isoformat() for n in range((end - start).days + 1)]


def build(conn, days=None, model=None, project=None):
    where, args = [], []
    if days:
        where.append("day >= ?")
        args.append((date.today() - timedelta(days=days - 1)).isoformat())
    if model:
        where.append("model = ?")
        args.append(model)
    if project:
        where.append("project = ?")
        args.append(project)
    sql = ("SELECT day, model, project, session_id, ts, input, output, cache_w5, cache_w1, cache_r, sidechain "
           "FROM messages" + (" WHERE " + " AND ".join(where) if where else ""))

    totals = dict(cost=0.0, input=0, output=0, cache_read=0, cache_write=0, messages=0)
    daily, models, projects, sessions = {}, {}, {}, {}
    unpriced = set()
    for day, mdl, proj, sess, ts, inp, out, w5, w1, rd, side in conn.execute(sql, args):
        c = pricing.cost(mdl, inp, out, w5, w1, rd)
        if c is None:
            unpriced.add(mdl)
        cv = c or 0.0
        totals["cost"] += cv
        totals["input"] += inp
        totals["output"] += out
        totals["cache_read"] += rd
        totals["cache_write"] += w5 + w1
        totals["messages"] += 1
        daily.setdefault(day, {})
        daily[day][mdl] = daily[day].get(mdl, 0.0) + cv

        m = models.setdefault(mdl, dict(model=mdl, input=0, output=0, cache_read=0, cache_write=0,
                                        messages=0, cost=0.0, priced=c is not None))
        m["input"] += inp; m["output"] += out; m["cache_read"] += rd
        m["cache_write"] += w5 + w1; m["messages"] += 1; m["cost"] += cv

        p = projects.setdefault(proj, dict(project=proj, cost=0.0, messages=0, sessions=set()))
        p["cost"] += cv; p["messages"] += 1; p["sessions"].add(sess)

        s = sessions.setdefault(sess, dict(session=sess, project=proj, first=ts, last=ts, messages=0,
                                           cost=0.0, models=set(), sidechain=0))
        s["first"] = min(s["first"], ts); s["last"] = max(s["last"], ts)
        s["messages"] += 1; s["cost"] += cv; s["models"].add(mdl); s["sidechain"] += side

    totals["sessions"] = len(sessions)
    day_list = _fill_days(daily.keys())
    for p in projects.values():
        p["sessions"] = len(p["sessions"])
    top = sorted(sessions.values(), key=lambda s: s["cost"], reverse=True)[:25]
    for s in top:
        s["models"] = sorted(s["models"])
        s["first"] = datetime.fromtimestamp(s["first"]).strftime("%Y-%m-%d %H:%M")
        s["last"] = datetime.fromtimestamp(s["last"]).strftime("%Y-%m-%d %H:%M")
    return dict(
        totals=totals,
        daily=[dict(day=d, total=sum(daily.get(d, {}).values()), models=daily.get(d, {})) for d in day_list],
        models=sorted(models.values(), key=lambda m: m["cost"], reverse=True),
        projects=sorted(projects.values(), key=lambda p: p["cost"], reverse=True)[:20],
        sessions=top,
        unpriced=sorted(unpriced),
    )
