void abort(void);
int aidandup(int oldfd);
int dup(int oldfd) { return aidandup(oldfd); }
int dup2(int oldfd, int newfd) { abort(); }
