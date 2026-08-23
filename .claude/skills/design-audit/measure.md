# Measurement snippets

All run through `em.sh js "<expr>"` (the wrapper wraps the expression in an
IIFE and `JSON.stringify`s the result) or `webview_execute_js`. Replace
`SEL` with the surface's anchor. Keep each call a single expression.

React note: a state read in the same call as a dispatched event sees the
pre-commit DOM. Read state in a *separate* `em.sh js` call.

## Geometry, background, scroll budget

```js
(() => { const p=document.querySelector('SEL'); const r=p.getBoundingClientRect(); const c=getComputedStyle(p);
  return { rect:[r.x,r.y,r.width,r.height].map(Math.round), scrollH:p.scrollHeight, clientH:p.clientHeight,
    screens:+(p.scrollHeight/p.clientHeight).toFixed(1), bg:c.backgroundColor, backdrop:c.webkitBackdropFilter||c.backdropFilter,
    pos:c.position, maxH:c.maxHeight, z:c.zIndex, vw:innerWidth, vh:innerHeight, dpr:devicePixelRatio }; })()
```

## Contrast of a text element against what's behind it

```js
(() => { const lum=s=>{const m=s.match(/[\d.]+/g).map(Number); const [r,g,b]=m.slice(0,3).map(v=>{v/=255;return v<=.03928?v/12.92:((v+.055)/1.055)**2.4}); return .2126*r+.7152*g+.0722*b};
  const bgOf=el=>{ while(el){ const c=getComputedStyle(el).backgroundColor; if(c&&!/rgba\(\d+, \d+, \d+, 0\)/.test(c)) return c; el=el.parentElement } return 'rgb(0,0,0)' };
  return [...document.querySelectorAll('SEL button, SEL .hint, SEL input, SEL span')].slice(0,40).map(el=>{ const c=getComputedStyle(el); const fg=c.color, bg=bgOf(el);
    const L1=lum(fg), L2=lum(bg); const ratio=(Math.max(L1,L2)+.05)/(Math.min(L1,L2)+.05);
    return { t:el.textContent.trim().slice(0,24), fg, bg, px:c.fontSize, ratio:+ratio.toFixed(2), aa: ratio>=4.5 }; })
  .filter(x=>x.t && !x.aa); })()
```

Alpha backgrounds are composited — for a translucent panel, compute against
the panel's solid fallback and say so.

## Hit-target census (interactive elements under 24px)

```js
(() => [...document.querySelectorAll('SEL button, SEL a, SEL input, SEL [role=button], SEL img[onclick], SEL .face-chip')]
  .map(el=>{const r=el.getBoundingClientRect(); return {t:(el.textContent||el.title||el.className).toString().trim().slice(0,20), w:Math.round(r.width), h:Math.round(r.height)}})
  .filter(x=>Math.min(x.w,x.h)<24 && x.w>0))()
```

HIG: compact Mac controls 22–28pt tall. WCAG 2.5.8: 24px. Anything over ~44px
in a panel row is oversized for a Mac control, too.

## Repeated-control census

```js
(() => { const p=document.querySelector('SEL'); const k={}; [...p.querySelectorAll('button,input,select,textarea,img')].forEach(e=>{const key=e.tagName+'.'+(e.className||'').toString().split(' ')[0]+(e.placeholder?'['+e.placeholder+']':''); k[key]=(k[key]||0)+1}); return k; })()
```

Fifteen identical inputs is a finding; this is how you count them.

## Focusable census and tab order

```js
(() => { const p=document.querySelector('SEL'); const all=[...p.querySelectorAll('button,input,select,textarea,a[href],[tabindex],img')];
  const tab=all.filter(e=>e.tabIndex>=0); const k={}; tab.forEach(e=>{const key=e.tagName+'.'+(e.className||'').toString().split(' ')[0]; k[key]=(k[key]||0)+1});
  const imgs=[...p.querySelectorAll('img')]; return { tabbable:tab.length, byKind:k, imgs:imgs.length, imgsFocusable:imgs.filter(i=>i.tabIndex>=0).length,
    imgsWithAlt:imgs.filter(i=>i.alt).length, clickableNonButtons:[...p.querySelectorAll('div,span,img')].filter(e=>e.onclick||getComputedStyle(e).cursor==='pointer').length }; })()
```

## Missing accessible names / live regions

```js
(() => { const p=document.querySelector('SEL'); return {
  unlabeledInputs:[...p.querySelectorAll('input')].filter(i=>!i.labels?.length && !i.getAttribute('aria-label') && !i.getAttribute('aria-labelledby')).map(i=>i.placeholder),
  iconButtons:[...p.querySelectorAll('button')].filter(b=>b.textContent.trim().length<=1 && !b.getAttribute('aria-label')).map(b=>b.textContent+'|'+b.title),
  liveRegions:document.querySelectorAll('[aria-live],[role=status],[role=alert]').length }; })()
```

## Does the surface cover a focusable control?

```js
(() => { const p=document.querySelector('SEL').getBoundingClientRect(); const hits=[];
  document.querySelectorAll('button,input,a[href]').forEach(b=>{ if(b.closest('SEL')) return; const r=b.getBoundingClientRect(); if(!r.width) return;
    const x=r.x+r.width/2, y=r.y+r.height/2; if(x<p.x||x>p.x+p.width||y<p.y||y>p.y+p.height) return;
    const top=document.elementFromPoint(x,y); if(top && !b.contains(top)) hits.push({covered:b.textContent.trim().slice(0,30), by:top.tagName+'.'+top.className}); });
  return hits; })()
```

## Broken images (chips, thumbs)

```js
(() => [...document.querySelectorAll('SEL img')].filter(i=>i.complete&&i.naturalWidth===0).map(i=>i.src.replace(/^photo:\/\/localhost\//,'')))()
```

Re-run minutes later: a retry counter that has reached its budget is a bug,
not a slow cache.

## Step-through click probe (for glitches like expand-then-rename)

Separate calls, in order; read state between them.

```sh
em.sh js "(() => { const el=document.querySelector('TARGET'); el.dispatchEvent(new MouseEvent('click',{bubbles:true})); return 'click'; })()"
em.sh js "(() => { const li=document.querySelector('CONTAINER'); return {expanded:!!li.querySelector('.faces'), editing:!!li.querySelector('input')}; })()"
```

Or real pointer input: `em.sh click 'SEL'` / `em.sh dblclick 'SEL'` (the CLI
resolves CSS selectors; tag a node with an `id` first via `js` if the
selector would be ambiguous).

## Typography and colour census (whole stylesheet)

```sh
grep -oE '#[0-9a-fA-F]{3,8}\b' src/App.css | sort | uniq -c | sort -rn      # literal colours vs the 5 tokens
grep -oE 'font-size: *[0-9.]+px' src/App.css | sort | uniq -c | sort -rn    # type scale actually in use
grep -c ':active' src/App.css; grep -c 'focus-visible' src/App.css           # press feedback, focus rings
grep -cE 'prefers-(contrast|reduced-transparency|reduced-motion)' src/App.css
```

## Escape / dismissal behaviour

```sh
em.sh key Escape; em.sh js "({open: !!document.querySelector('SEL'), active: document.activeElement.tagName+'.'+document.activeElement.className})"
```

## Crop for the report

```sh
em.sh crop <shot-name> <x> <y> <w> <h> <ev-name>    # pixel coords in the 2× screenshot; output ≤ 560px wide JPEG in $AUDIT_DIR/ev
```

A panel at CSS `x=1128,w=300` is at `x=2256,w=600` in the screenshot
(`devicePixelRatio` 2).
