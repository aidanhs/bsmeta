#include <stdint.h>
#include <string.h>
#include "quickjs/quickjs-libc.h"
#include "quickjs/quickjs.h"

// Stolen from qjs.c
static int eval_buf(JSContext *ctx, const void *buf, int buf_len,
                    const char *filename, int eval_flags)
{
    JSValue val;
    int ret;

    if ((eval_flags & JS_EVAL_TYPE_MASK) == JS_EVAL_TYPE_MODULE) {
        /* for the modules, we compile then run to be able to set
           import.meta */
        val = JS_Eval(ctx, buf, buf_len, filename,
                      eval_flags | JS_EVAL_FLAG_COMPILE_ONLY);
        if (!JS_IsException(val)) {
            js_module_set_import_meta(ctx, val, 1, 1);
            val = JS_EvalFunction(ctx, val);
        }
    } else {
        val = JS_Eval(ctx, buf, buf_len, filename, eval_flags);
    }
    if (JS_IsException(val)) {
        js_std_dump_error(ctx);
        ret = -1;
    } else {
        ret = 0;
    }
    JS_FreeValue(ctx, val);
    return ret;
}

int run_script() {
    JSRuntime *rt;
    JSContext *ctx;

    rt = JS_NewRuntime();
    if (!rt) {
        fprintf(stderr, "qjs: cannot allocate JS runtime\n");
        return 2;
    }

    ctx = JS_NewContext(rt);
    if (!ctx) {
        fprintf(stderr, "qjs: cannot allocate JS context\n");
        return 2;
    }
    js_init_module_std(ctx, "std");
    js_init_module_os(ctx, "os");
    js_std_add_helpers(ctx, -1, NULL);

    int ret;

    const char *base =
        "import * as std from 'std';\n"
        "import * as os from 'os';\n"
        "globalThis.std = std;\n"
        "globalThis.os = os;\n";

    ret = eval_buf(ctx, base, strlen(base), "<input>", JS_EVAL_TYPE_MODULE);
    if (ret != 0) { return ret; }

    const char *filename = "/work/script.js";
    size_t buf_len;
    uint8_t *buf = js_load_file(ctx, &buf_len, filename);
    if (!buf) {
        perror(filename);
        return 1;
    }

    ret = eval_buf(ctx, buf, buf_len, "<input>", JS_EVAL_TYPE_MODULE);
    js_free(ctx, buf);

    if (ret != 0) { return ret; }

    return 0;
}

int main(int argc, char **argv) {
    setbuf(stdout, NULL);
    setbuf(stderr, NULL);
    return run_script();
    // TODO, or figure how to get return val from main
    //exit(run_script());
}
