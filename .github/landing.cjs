// Builds a one-page project site from README.md. Usage: node .github/landing.cjs <outdir> [Owner/repo] [branch]
const fs = require('node:fs');
const path = require('node:path');
const { marked } = require('marked');

const out = process.argv[2] || '_landing';
const slugRepo = process.argv[3] || process.env.GITHUB_REPOSITORY || 'Gitguy268/' + path.basename(process.cwd());
const branch = process.argv[4] || process.env.GITHUB_REF_NAME || 'main';
const raw = `https://raw.githubusercontent.com/${slugRepo}/${branch}/`;
const blob = `https://github.com/${slugRepo}/blob/${branch}/`;
const md = fs.readFileSync('README.md', 'utf8');

const seen = new Map();
const slug = (s) => {
  const base = s.replace(/<[^>]+>/g, '').toLowerCase().replace(/[^\p{L}\p{N}\s-]/gu, '').trim().replace(/\s+/g, '-');
  const n = seen.get(base) || 0;
  seen.set(base, n + 1);
  return n ? `${base}-${n}` : base;
};
marked.use({
  renderer: {
    heading({ tokens, depth }) {
      const text = this.parser.parseInline(tokens);
      return `<h${depth} id="${slug(text)}">${text}</h${depth}>\n`;
    },
  },
});

const absolutize = (html) =>
  html
    .replace(/\b(src|srcset)="(?!https?:|data:|#|\/\/)\.?\/?([^"]+)"/g, (_, a, p) => `${a}="${raw}${p}"`)
    .replace(/\bhref="(?!https?:|mailto:|#|\/\/)\.?\/?([^"]+)"/g, (_, p) => `href="${blob}${p}"`);

const body = absolutize(marked.parse(md));
// Use the first real heading (a `# comment` inside a code block doesn't count), then the repo name.
const firstHeading = marked.lexer(md).find((t) => t.type === 'heading');
const htmlHeading = md.match(/<h1[^>]*>([^<]+)<\/h1>/i);
const title = (firstHeading ? firstHeading.text : htmlHeading ? htmlHeading[1] : path.basename(slugRepo)).replace(/<[^>]+>/g, '').replace(/[*_`]/g, '').trim() || path.basename(slugRepo);
const firstPara = (body.match(/<p>([\s\S]*?)<\/p>/) || [, ''])[1].replace(/<[^>]+>/g, '').replace(/"/g, '&quot;').slice(0, 160);

const page = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${title}</title>
<meta name="description" content="${firstPara}">
<style>
:root { color-scheme: light dark; --bg:#fff; --fg:#1f2328; --muted:#59636e; --line:#d1d9e0; --code:#f6f8fa; --link:#0550ae; }
@media (prefers-color-scheme: dark) { :root { --bg:#0d1117; --fg:#e6edf3; --muted:#9198a1; --line:#30363d; --code:#161b22; --link:#79c0ff; } }
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg); font:16px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif; }
main { max-width:860px; margin:0 auto; padding:24px 16px 64px; overflow-wrap:break-word; }
.bar { display:flex; justify-content:space-between; gap:12px; flex-wrap:wrap; padding-bottom:12px; margin-bottom:24px; border-bottom:1px solid var(--line); font-size:14px; color:var(--muted); }
a { color:var(--link); }
img { max-width:100%; height:auto; }
pre { background:var(--code); padding:14px 16px; border-radius:8px; overflow:auto; }
code { background:var(--code); padding:.15em .35em; border-radius:4px; font:.88em ui-monospace,SFMono-Regular,Menlo,monospace; }
pre code { background:none; padding:0; }
table { display:block; overflow:auto; border-collapse:collapse; }
th,td { border:1px solid var(--line); padding:6px 12px; text-align:left; }
blockquote { margin:16px 0; padding:0 16px; border-left:4px solid var(--line); color:var(--muted); }
h1,h2 { border-bottom:1px solid var(--line); padding-bottom:.3em; line-height:1.25; }
summary { cursor:pointer; font-weight:600; }
</style>
</head>
<body>
<main>
<div class="bar"><span>Project page</span><a href="https://github.com/${slugRepo}">View on GitHub</a></div>
${body}
</main>
</body>
</html>
`;
fs.mkdirSync(out, { recursive: true });
fs.writeFileSync(path.join(out, 'index.html'), page);
fs.writeFileSync(path.join(out, '.nojekyll'), '');
