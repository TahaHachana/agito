(function(kind, attr, rawText, exact, pierce) {
    function norm(s){return String(s).replace(/\s+/g,' ').trim();}
    var want=norm(rawText);
    var wantLower=want.toLowerCase();
    function matches(s){
        if(s==null) return false;
        s=norm(s);
        return exact ? s===want : s.toLowerCase().indexOf(wantLower)!==-1;
    }
    var SKIP={SCRIPT:1,STYLE:1,NOSCRIPT:1,TEMPLATE:1};
    var cache=new Map();
    function textOf(node){
        if(cache.has(node)) return cache.get(node);
        var s='';
        for(var c=node.firstChild;c;c=c.nextSibling){
            if(c.nodeType===3) s+=c.nodeValue;
            else if(c.nodeType===1 && !SKIP[c.nodeName]) s+=textOf(c);
        }
        if(pierce && node.shadowRoot) s+=textOf(node.shadowRoot);
        cache.set(node,s);
        return s;
    }
    var all=[];
    function collect(root){
        var els=root.querySelectorAll('*');
        for(var i=0;i<els.length;i++){
            all.push(els[i]);
            if(pierce && els[i].shadowRoot) collect(els[i].shadowRoot);
        }
    }
    collect(document);
    function isValueButton(el){
        if(el.nodeName!=='INPUT') return false;
        var t=(el.getAttribute('type')||'').toLowerCase();
        return t==='button'||t==='submit'||t==='reset';
    }
    function textMatches(el){
        if(SKIP[el.nodeName] || (document.head && document.head.contains(el))) return false;
        if(isValueButton(el)) return matches(el.value);
        if(!matches(textOf(el))) return false;
        var kids=Array.from(el.children);
        if(pierce && el.shadowRoot) kids=kids.concat(Array.from(el.shadowRoot.children));
        for(var i=0;i<kids.length;i++){
            if(!SKIP[kids[i].nodeName] && matches(textOf(kids[i]))) return false;
        }
        return true;
    }
    function labelsOf(el){
        var ids=(el.getAttribute('aria-labelledby')||'').split(/\s+/).filter(Boolean);
        if(ids.length){
            var root=el.getRootNode();
            var parts=[];
            for(var i=0;i<ids.length;i++){
                var ref=root.getElementById ? root.getElementById(ids[i]) : document.getElementById(ids[i]);
                if(ref) parts.push(textOf(ref));
            }
            if(parts.length) return parts.length>1 ? parts.concat([parts.join(' ')]) : parts;
        }
        var aria=el.getAttribute('aria-label');
        if(aria && aria.trim()) return [aria];
        if(el.labels && !(el.nodeName==='INPUT' && el.type==='hidden'))
            return Array.from(el.labels).map(textOf);
        return [];
    }
    return all.filter(function(el){
        if(kind==='text') return textMatches(el);
        if(kind==='label') return labelsOf(el).some(matches);
        return el.hasAttribute(attr) && matches(el.getAttribute(attr));
    });
})
