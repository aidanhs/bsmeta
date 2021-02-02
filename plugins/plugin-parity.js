// Note that parity is tricky and may sometimes flag maps wrongly - ideally we'll be able to catch these via
// curators or by people messaging in #curation-request on discord.
//
// The rules in this script are as follows (credit to Pyrowarfare).
//
// Difficulties are considered like so:
//  - easy, normal maps are ignored entirely - beginner players reset every swing so never follow parity
//  - hard, expert, expert+ maps are all requires to 'pass' parity
//
//  A map passing parity means:
//  - full errors are insta-fail - only very skilled mappers will deliberately break parity
//  - warnings have a max of 10 - we may tweak this over time, depending on what gets flagged
//

import * as std from 'std';
import * as os from 'os';
globalThis.std = std;
globalThis.os = os;

function noop() {}

// Override console.log to use stderr - stdout is how we're going to give output to bsmeta
var oldLog = console.log;
console.log = function () {
    var strings = [];
    for (var i = 0; i < arguments.length; i++) {
        strings.push(arguments[i].toString());
    }
    std.err.puts('stdout:' + strings.join(' ') + '\n');
}

function getScrollLineHeight() {
    const el = document.createElement('div');
    el.style.fontSize = 'initial';
    el.style.display = 'none';
    document.body.appendChild(el);
    const fontSize = window.getComputedStyle(el).fontSize;
    document.body.removeChild(el);
    return fontSize ? window.parseInt(fontSize) : 16;
}
// To handle getScrollLineHeight
var SUMMARY = {};
globalThis.document = {
    createElement: () => ({ 'style': {} }),
    body: {
        appendChild: noop,
        removeChild: noop,
    },
    getElementById: (id) => {
        if (id !== 'summary') { throw 'oh no'; }
        return SUMMARY;
    },
};
globalThis.window = {
    getComputedStyle: () => 0,
    parseInt: noop,
};

std.loadScript("/work/bs-parity-main.js");

function analyseMap(mappath) {
    // Post-load overrides
    var TOTAL_OUTPUT = [];
    outputUI = function (note, parity, message, messageType, persistent = false) {
        TOTAL_OUTPUT.push([note, parity, message, messageType]);
    };
    clearOutput = noop;

    // Prep globals
    let parsed = JSON.parse(std.loadFile(mappath));
    notesArray = getNotes(parsed);
    wallsArray = getWalls(parsed);
    ready = true;

    // Do the calc
    checkParity();

    // Return
    return {
        num_errors: TOTAL_OUTPUT.filter((e) => e[3] === 'error').length,
        num_warnings: TOTAL_OUTPUT.filter((e) => e[3] === 'warning').length,
    };
}

let infodat = JSON.parse(std.loadFile("/data/info.dat"));
let failed = [];
infodat._difficultyBeatmapSets.forEach((diffSet) => {
    let setName = diffSet._beatmapCharacteristicName;
    diffSet._difficultyBeatmaps.forEach((diff) => {
        let d = diff._difficulty;
        if (d === "Easy" || d === "Normal") {
            return;
        }
        if (d !== "Hard" && d !== "Expert" && d !== "ExpertPlus") {
            throw 'Unknown difficulty';
        }
        let res = analyseMap('/data/' + diff._beatmapFilename);
        if (res.num_errors > 0) {
            failed.push(setName + ':' + d + ' had ' + res.num_errors + ' errors');
        }
        if (res.num_warnings > 10) {
            failed.push(setName + ':' + d + ' had ' + res.num_warnings + ' warnings');
        }
    });
});
std.out.puts(JSON.stringify({
    failed: failed.length !== 0,
    whyfailed: failed.join(', '),
}));
