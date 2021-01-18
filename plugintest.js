import * as std from 'std';
import * as os from 'os';
globalThis.std = std;
globalThis.os = os;

console.log('hmm');
console.log(os.readdir('/'));
console.log('hmm2');

function noop() {}

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

//std.loadScript("/work/bs-parity/scripts/main.js");
std.loadScript("/work/bs-parity-main.js");
// Post-load overrides
var TOTAL_OUTPUT = [];
outputUI = function (note, parity, message, messageType, persistent = false) {
    TOTAL_OUTPUT.push([note, parity, message, messageType]);
};
clearOutput = noop;

// Prep globals
//let parsed = JSON.parse(std.loadFile("../beatmaps/7f0356d54ded74ed2dbf56e7290a29fde002c0af/ExpertPlusStandard.dat"));
let parsed = JSON.parse(std.loadFile("/work/7f0356d54ded74ed2dbf56e7290a29fde002c0af-ExpertPlusStandard.dat"));
notesArray = getNotes(parsed);
wallsArray = getWalls(parsed);
ready = true;

// Do the calc
checkParity();

// Return
console.log(JSON.stringify(TOTAL_OUTPUT));
console.log(JSON.stringify(SUMMARY.textContent));
