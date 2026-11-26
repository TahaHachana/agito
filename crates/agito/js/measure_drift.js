function() {
    var r1 = this.getBoundingClientRect();
    var self = this;
    return new Promise(function(resolve) {
        setTimeout(function() {
            var r2 = self.getBoundingClientRect();
            var d = Math.abs(r1.x-r2.x) + Math.abs(r1.y-r2.y) +
                    Math.abs(r1.width-r2.width) + Math.abs(r1.height-r2.height);
            resolve(d);
        }, 50);
    });
}
