/*
 * Corvus GCS macOS launcher: Contents/MacOS/corvus-gcs.
 *
 * A native executable on purpose. The launcher used to be a bash script, and
 * LaunchServices cannot read an architecture off a script: on Apple Silicon it
 * started it under Rosetta, the universal interpreter followed into x86_64,
 * and the arm64-only Qt in the bundle failed to load, so a double-clicked app
 * died before its first window. On a Mac without Rosetta it asked to install
 * Rosetta instead. Compiled by build-macos-app.sh for the bundle's own
 * architecture, so the process is native from the first instruction.
 *
 * It sets what the interpreter needs to find itself inside the bundle, wherever
 * the bundle is (translocated, on a read-only disk image), and execs it, so
 * SIGINT and SIGTERM reach corvus/app.py directly and its handlers tear down
 * cleanly.
 */
#include <dirent.h>
#include <limits.h>
#include <mach-o/dyld.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int fail(const char *what, const char *detail)
{
    fprintf(stderr, "corvus-gcs: %s%s%s\n", what, detail ? ": " : "", detail ? detail : "");
    return 1;
}

static int drop_last(char *path)
{
    char *slash = strrchr(path, '/');
    if (!slash || slash == path)
        return -1;
    *slash = '\0';
    return 0;
}

int main(int argc, char *argv[])
{
    char raw[PATH_MAX];
    char contents[PATH_MAX];
    uint32_t size = sizeof raw;
    if (_NSGetExecutablePath(raw, &size) != 0 || !realpath(raw, contents))
        return fail("cannot locate the launcher", NULL);
    /* .../Contents/MacOS/corvus-gcs -> .../Contents */
    if (drop_last(contents) || drop_last(contents))
        return fail("unexpected launcher path", contents);

    char res[PATH_MAX], app[PATH_MAX], home[PATH_MAX], lib[PATH_MAX];
    snprintf(res, sizeof res, "%s/Resources", contents);
    snprintf(app, sizeof app, "%s/app", res);
    snprintf(home, sizeof home, "%s/python", res);
    snprintf(lib, sizeof lib, "%s/lib", home);

    /* pythonX.Y was fixed at build time; read it back rather than repeat it. */
    char version[NAME_MAX + 1] = "";
    DIR *dir = opendir(lib);
    if (!dir)
        return fail("bundled python not found under", lib);
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strncmp(entry->d_name, "python3.", 8) != 0)
            continue;
        if (!version[0] || strcmp(entry->d_name, version) < 0)
            snprintf(version, sizeof version, "%s", entry->d_name);
    }
    closedir(dir);
    if (!version[0])
        return fail("bundled python3.x not found under", lib);

    char path[2 * PATH_MAX + 2], python[PATH_MAX], script[PATH_MAX];
    snprintf(path, sizeof path, "%s:%s/%s/site-packages", app, lib, version);
    snprintf(python, sizeof python, "%s/bin/python3", home);
    snprintf(script, sizeof script, "%s/corvus/app.py", app);

    /* PYTHONHOME overrides the stale pyvenv.cfg `home =`; PYTHONPATH re-adds
     * the site-packages PYTHONHOME bypasses, and the app root for `corvus`.
     * No bytecode: a .pyc written into the bundle breaks its signature, and
     * the next launch of a downloaded copy is refused as damaged. */
    setenv("PYTHONHOME", home, 1);
    setenv("PYTHONPATH", path, 1);
    setenv("PYTHONDONTWRITEBYTECODE", "1", 1);
    if (chdir(app) != 0)
        return fail("cannot enter", app);

    char **args = calloc((size_t)argc + 2, sizeof *args);
    if (!args)
        return fail("out of memory", NULL);
    args[0] = python;
    args[1] = script;
    for (int i = 1; i < argc; i++)
        args[i + 1] = argv[i];
    execv(python, args);
    perror("corvus-gcs: exec");
    return 1;
}
