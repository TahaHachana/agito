function(values) {
    const options = Array.from(this.options);
    for (const opt of options) {
        opt.selected = values.includes(opt.value) || values.includes(opt.label);
    }
    // Read back once every option is set: changing one can reset another in a single select.
    const selected = options.filter(o => o.selected).map(o => o.value);
    this.dispatchEvent(new Event('input', { bubbles: true }));
    this.dispatchEvent(new Event('change', { bubbles: true }));
    return selected;
}
