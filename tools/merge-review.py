"""Merge a phone review pass back into a Roughcut project.

The review page (an Artifact, `capabilities: {db: {}}`) keeps one document
per clip under `reviews/<clip id>`, holding what was kept and what was
archived. Claude reads those documents out with the Artifact tool's
`read_db --out_dir`, which writes one JSON file per clip; this applies them.

    python tools/merge-review.py <reviews-dir> --project <file> [--apply]

Without --apply it only reports, which is the point: a review pass is a lot
of judgement to hand to a script sight-unseen.

Clips are matched by id, never by filename -- two files can share a name
across folders, and an id is what the page was given.
"""
import argparse, json, os, shutil, sys, time


def inclusive_len(a, b):
    return b - a + 1


def keep(highlights, a, b, last):
    """Insert a stretch, merging what it overlaps or touches.

    The same rule as `SourceClip::keep` in the model crate: the list stays
    sorted and disjoint, so "the stretch covering this frame" always has
    exactly one answer.
    """
    a, b = max(0, min(a, b)), min(last, max(a, b))
    if b < a:
        return highlights
    merged = []
    for h in highlights:
        if h['out_frame'] + 1 < a or h['in_frame'] > b + 1:
            merged.append(h)
        else:
            a = min(a, h['in_frame'])
            b = max(b, h['out_frame'])
    merged.append({'in_frame': a, 'out_frame': b})
    merged.sort(key=lambda h: h['in_frame'])
    return merged


def load_reviews(path):
    """Every JSON document under `path`, keyed by the clip id it names."""
    out = {}
    for root, _dirs, files in os.walk(path):
        for name in files:
            if not name.endswith('.json'):
                continue
            full = os.path.join(root, name)
            try:
                body = json.load(open(full, encoding='utf-8'))
            except (ValueError, OSError) as e:
                print(f'  skipped {name}: {e}', file=sys.stderr)
                continue
            # read_db --out_dir names each file for its document id, which
            # is the clip id the page wrote under.
            out[os.path.splitext(name)[0]] = body
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('reviews', help='directory of review documents')
    ap.add_argument('--project', required=True)
    ap.add_argument('--apply', action='store_true',
                    help='write the project; without it, only report')
    args = ap.parse_args()

    project = json.load(open(args.project, encoding='utf-8'))
    reviews = load_reviews(args.reviews)
    if not reviews:
        print(f'no review documents under {args.reviews}')
        return 1

    by_id = {c['id']: c for c in project['clips']}
    kept_total = archived = restored = unknown = 0
    lines = []

    for clip_id, body in reviews.items():
        clip = by_id.get(clip_id)
        if clip is None:
            unknown += 1
            print(f'  ! no clip {clip_id} ({body.get("name", "?")}) in this project')
            continue
        name = clip['path'].split('/')[-1]
        last = max(0, clip['duration_frames'] - 1)
        note = []

        was_archived = bool(clip.get('archived', False))
        now_archived = bool(body.get('archived', False))
        if now_archived != was_archived:
            clip['archived'] = now_archived
            if now_archived:
                archived += 1
                note.append('archived')
            else:
                restored += 1
                note.append('restored')

        highlights = list(clip.get('highlights', []))
        before = len(highlights)
        for h in body.get('highlights', []):
            try:
                a, b = int(h['in']), int(h['out'])
            except (KeyError, TypeError, ValueError):
                continue
            highlights = keep(highlights, a, b, last)
        if highlights != clip.get('highlights', []):
            clip['highlights'] = highlights
            added = len(highlights) - before
            frames = sum(inclusive_len(h['in_frame'], h['out_frame']) for h in highlights)
            kept_total += max(0, added)
            note.append(f'{len(highlights)} stretch(es), {frames} frames'
                        + (f' (+{added})' if added > 0 else ' (merged)'))
        if note:
            lines.append(f'  {name:24} {", ".join(note)}')

    for line in sorted(lines):
        print(line)
    print(f'\n{len(reviews)} reviewed | {kept_total} new stretch(es) | '
          f'{archived} archived | {restored} restored'
          + (f' | {unknown} unmatched' if unknown else ''))

    if not args.apply:
        print('\nnothing written — pass --apply to write the project')
        return 0

    backup = f'{args.project}.bak-{time.strftime("%Y%m%d-%H%M%S")}'
    shutil.copy2(args.project, backup)
    with open(args.project, 'w', encoding='utf-8') as f:
        json.dump(project, f, indent=2)
        f.write('\n')
    print(f'\nwrote {args.project}\nprevious version kept at {backup}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
