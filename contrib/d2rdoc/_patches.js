// Schema patches applied after all schema files load.
// Fixes incorrect cross-reference targets without modifying upstream schema files.

// skills.passiveitype: schema incorrectly points to itemtypes.itemtype;
// the correct lookup column is itemtypes.code.
(function () {
    // Example:
    // skills.passiveitype: schema incorrectly points to itemtypes.itemtype;
    // the correct lookup column is itemtypes.code.
    
    /*var skills = files["skills"];
    if (!skills) return;
    var field = skills.fields.find(function (f) { return f.name === "passiveitype"; });
    if (!field || !field.type) return;
    field.type.field = "code";*/
})();
