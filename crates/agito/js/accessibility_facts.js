// What an accessibility audit needs from a document that its accessibility tree does not
// carry: the document's language, ids used more than once, and visible text with the colours
// and font behind it. Prepended with css_path.js.
(function () {
    var MAX_SAMPLES = 2000;
    var counts = {};
    var samples = [];

    function visible(el, style) {
        return el.getClientRects().length > 0 &&
            style.visibility !== 'hidden' &&
            parseFloat(style.opacity) > 0;
    }

    // Text in a disabled control is exempt from contrast requirements.
    function disabled(el) {
        return !!el.closest('button:disabled, input:disabled, select:disabled, textarea:disabled, fieldset:disabled, [aria-disabled="true"]');
    }

    function ownText(el) {
        var text = '';
        for (var c = el.firstChild; c; c = c.nextSibling) {
            if (c.nodeType === 3) text += c.nodeValue;
        }
        return text.replace(/\s+/g, ' ').trim();
    }

    // Backgrounds from the element up, nearest first, stopping at the first opaque one, which
    // computed styles serialize as rgb(); whether an image or gradient lies in between.
    function backgrounds(el) {
        var layers = [];
        var image = false;
        for (var node = el; node && node.nodeType === 1; node = node.parentElement || (node.getRootNode().host || null)) {
            var style = getComputedStyle(node);
            if (style.backgroundImage && style.backgroundImage !== 'none') image = true;
            layers.push(style.backgroundColor);
            if (/^rgb\(/.test(style.backgroundColor)) break;
        }
        return { layers: layers, image: image };
    }

    function visit(root) {
        var all = root.querySelectorAll('*');
        for (var i = 0; i < all.length; i++) {
            var el = all[i];
            if (el.id) counts[el.id] = (counts[el.id] || 0) + 1;
            if (el.shadowRoot) visit(el.shadowRoot);
            if (samples.length >= MAX_SAMPLES) continue;
            var text = ownText(el);
            if (!text || /^(script|style|noscript|template|title|option)$/.test(el.localName)) continue;
            var style = getComputedStyle(el);
            if (!visible(el, style) || disabled(el)) continue;
            var bg = backgrounds(el);
            samples.push({
                selector: cssPath(el),
                text: text.length > 40 ? text.slice(0, 40) + '…' : text,
                color: style.color,
                backgrounds: bg.layers,
                backgroundImage: bg.image,
                fontSize: parseFloat(style.fontSize) || 16,
                fontWeight: parseFloat(style.fontWeight) || 400
            });
        }
    }

    visit(document);
    return {
        language: document.documentElement.getAttribute('lang') || null,
        duplicateIds: Object.keys(counts).filter(function (id) { return counts[id] > 1; }),
        text: samples
    };
})()
