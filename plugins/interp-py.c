#include <stdint.h>
#include <string.h>
//#include "cpython/lib.zip.h"
#include "cpython/Include/Python.h"

char *ttyname(int fd) { abort(); }
int system(const char *command) { abort(); }
int execv(const char *path, char *const argv[]) { abort(); }
int execve(const char *filename, char *const argv[], char *const envp[]) { abort(); }
pid_t fork(void) { abort(); }
int unlockpt(int fd) { abort(); }
char *ptsname(int fd) { abort(); }
pid_t getppid(void) { abort(); }
int kill(pid_t pid, int sig) { abort(); }
pid_t wait(int *wstatus) { abort(); }
int pipe(int pipefd[2]) { abort(); }

int run_script() {
    int ret;

    // In theory Py_HashRandomizationFlag exists, but it doesn't do anything!
    ret = setenv("PYTHONHASHSEED", "0", 1);
    if (ret != 0) {
        perror("set python hash seed");
        return ret;
    }
    ret = setenv("PYTHONHOME", "/homeless", 1);
    if (ret != 0) {
        perror("set python home");
        return ret;
    }
    ret = setenv("PYTHONPATH", "/work/lib.zip", 1);
    if (ret != 0) {
        perror("set python path");
        return ret;
    }

    Py_NoSiteFlag = 1; // TODO: needs symlinks to enable this...but don't *really* need it, since no site packages
    Py_VerboseFlag = 0;
    Py_DebugFlag = 0;
    Py_DontWriteBytecodeFlag = 1;
    Py_UnbufferedStdioFlag = 1;

    Py_InitializeEx(0); // don't initialize signals

    const char *filename = "/work/script.py";
    FILE *fp = fopen(filename, "r");
    if (!fp) {
        perror(filename);
        return 1;
    }
    ret = PyRun_SimpleFile(fp, filename);

    Py_Finalize();

    return ret;
}

int main(int argc, char **argv) {
    setbuf(stdout, NULL);
    setbuf(stderr, NULL);
    return run_script();
    // TODO, or figure how to get return val from main
    //exit(run_script());
}
