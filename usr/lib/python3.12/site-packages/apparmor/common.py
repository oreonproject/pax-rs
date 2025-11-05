# ------------------------------------------------------------------
#
#    Copyright (C) 2012 Canonical Ltd.
#    Copyright (C) 2013 Kshitij Gupta <kgupta8592@gmail.com>
#
#    This program is free software; you can redistribute it and/or
#    modify it under the terms of version 2 of the GNU General Public
#    License published by the Free Software Foundation.
#
# ------------------------------------------------------------------

import collections
import glob
import logging
import os
import re
import subprocess
import sys
import termios
import tty
from tempfile import NamedTemporaryFile

import apparmor.rules as rules

DEBUGGING = False


#
# Utility classes
#
class AppArmorException(Exception):
    """This class represents AppArmor exceptions"""
    def __init__(self, value):
        self.value = value

    def __str__(self):
        return repr(self.value)


class AppArmorBug(Exception):
    """This class represents AppArmor exceptions "that should never happen"."""


#
# Utility functions
#
def error(out, exit_code=1, do_exit=True):
    """Print error message and exit"""
    try:
        print("ERROR: %s" % (out), file=sys.stderr)
    except IOError:
        pass

    if do_exit:
        sys.exit(exit_code)


def warn(out):
    """Print warning message"""
    try:
        print("WARN: %s" % (out), file=sys.stderr)
    except IOError:
        pass


def msg(out, output=sys.stdout):
    """Print message"""
    try:
        print("%s" % (out), file=output)
    except IOError:
        pass


def debug(out):
    """Print debug message"""
    global DEBUGGING
    if DEBUGGING:
        try:
            print("DEBUG: %s" % (out), file=sys.stderr)
        except IOError:
            pass


def recursive_print(src, dpth=0, key=''):
    # print recursively in a nicely formatted way
    # useful for debugging, too verbose for production code ;-)

    # based on code "stolen" from Scott S-Allen / MIT License
    # http://code.activestate.com/recipes/578094-recursively-print-nested-dictionaries/
    """Recursively prints nested elements."""
    tabs = ' ' * dpth * 4  # or 2 or 8 or...

    if isinstance(src, dict):
        empty = True
        for key in src.keys():
            print(tabs + '[%s]' % key)
            recursive_print(src[key], dpth + 1, key)
            empty = False
        if empty:
            print(tabs + '[--- empty ---]')
    elif isinstance(src, list) or isinstance(src, tuple):
        if not src:
            print(tabs + '[--- empty ---]')
        else:
            print(tabs + "[")
            for litem in src:
                recursive_print(litem, dpth + 1)
            print(tabs + "]")
    elif isinstance(src, rules._Raw_Rule):
        src.recursive_print(dpth)
    else:
        if key:
            print(tabs + '%s = %s' % (key, src))
        else:
            print(tabs + '- %s' % src)


def cmd(command):
    """Try to execute the given command."""
    debug(command)
    try:
        sp = subprocess.Popen(command, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT)
    except OSError as ex:
        return [127, str(ex)]

    out = sp.communicate()[0].decode('ascii', 'ignore')

    return [sp.returncode, out]


def cmd_pipe(command1, command2):
    """Try to pipe command1 into command2."""
    try:
        sp1 = subprocess.Popen(command1, stdout=subprocess.PIPE)
        sp2 = subprocess.Popen(command2, stdin=sp1.stdout)
    except OSError as ex:
        return [127, str(ex)]

    out = sp2.communicate()[0].decode('ascii', 'ignore')

    return [sp2.returncode, out]


def valid_path(path):
    """Valid path"""
    # No relative paths
    m = "Invalid path: %s" % (path)
    if not path.startswith('/'):
        debug("%s (relative)" % (m))
        return False

    if '"' in path:  # We double quote elsewhere
        debug("%s (contains quote)" % (m))
        return False

    try:
        os.path.normpath(path)
    except Exception:
        debug("%s (could not normalize)" % (m))
        return False
    return True


def get_directory_contents(path):
    """Find contents of the given directory"""
    if not valid_path(path):
        return None

    files = []
    for f in glob.glob(path + "/*"):
        files.append(f)

    files.sort()
    return files


def is_skippable_file(path):
    """Returns True if filename matches something to be skipped (rpm or dpkg backup files, hidden files etc.)
        The list of skippable files needs to be synced with apparmor initscript and libapparmor _aa_is_blacklisted()
        path: filename (with or without directory)"""

    basename = os.path.basename(path)

    if not basename or basename.startswith('.') or basename == 'README':
        return True

    skippable_suffix = (
        '.dpkg-new', '.dpkg-old', '.dpkg-dist', '.dpkg-bak', '.dpkg-remove',
        '.pacsave', '.pacnew', '.rpmnew', '.rpmsave', '.orig', '.rej', '~')
    if basename.endswith(skippable_suffix):
        return True

    return False


def open_file_read(path, encoding='UTF-8'):
    """Open specified file read-only"""
    return open_file_anymode('r', path, encoding)


def open_file_write(path):
    """Open specified file in write/overwrite mode"""
    return open_file_anymode('w', path, 'UTF-8')


def open_file_anymode(mode, path, encoding='UTF-8'):
    """Crash-resistant wrapper to open a specified file in specified mode"""

    # This avoids a crash when reading a logfile with special characters that
    # are not utf8-encoded (for example a latin1 "ö"), and also avoids crashes
    # at several other places we don't know yet ;-)
    return open(path, mode, encoding=encoding, errors='surrogateescape')


def readkey():
    """Returns the pressed key"""
    fd = sys.stdin.fileno()
    old_settings = termios.tcgetattr(fd)
    try:
        tty.setraw(sys.stdin.fileno())
        ch = sys.stdin.read(1)
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old_settings)

    return ch


def hasher():
    """A neat alternative to perl's hash reference"""
    # Creates a dictionary for any depth and returns empty dictionary otherwise
    # WARNING: when reading non-existing sub-dicts, empty dicts will be added.
    #          This might cause strange effects when using .keys()
    return collections.defaultdict(hasher)


def convert_regexp(regexp):
    regex_paren = re.compile('^(.*){([^}]*)}(.*)$')
    regexp = regexp.strip()

    regexp = regexp.replace('(', '\\(').replace(')', '\\)')  # escape '(' and ')'

    new_reg = re.sub(r'(?<!\\)([.+$])', r'\\\1', regexp)

    while regex_paren.search(new_reg):
        match = regex_paren.search(new_reg).groups()
        prev = match[0]
        after = match[2]
        p1 = match[1].replace(',', '|')
        new_reg = prev + '(' + p1 + ')' + after

    new_reg = new_reg.replace('?', '[^/\000]')

    multi_glob = '__KJHDKVZH_AAPROF_INTERNAL_GLOB_SVCUZDGZID__'
    new_reg = new_reg.replace('**', multi_glob)
    # print(new_reg)

    # Match at least one character if * or ** after /
    # ?< is the negative lookback operator
    new_reg = new_reg.replace('*', '(((?<=/)[^/\000]+)|((?<!/)[^/\000]*))')
    new_reg = new_reg.replace(multi_glob, '(((?<=/)[^\000]+)|((?<!/)[^\000]*))')
    if not regexp.startswith('^'):
        new_reg = '^' + new_reg
    if not regexp.endswith('$'):
        new_reg = new_reg + '$'
    return new_reg


def user_perm(prof_dir):
    if not os.access(prof_dir, os.W_OK):
        sys.stdout.write("Cannot write to profile directory.\n"
                         "Please run as a user with appropriate permissions.\n")
        return False
    return True


def split_name(full_profile):
    if '//' in full_profile:
        profile, hat = full_profile.split('//')[:2]  # XXX limit to two levels to avoid an Exception on nested child profiles or nested null-*
        # TODO: support nested child profiles
    else:
        profile = full_profile
        hat = full_profile

    return (profile, hat)


def combine_profname(name_parts):
    """combine name_parts (main profile, child) into a joint main//child profile name"""

    if type(name_parts) is not list:
        raise AppArmorBug('combine_name() called with parameter of type %s, must be a list' % type(name_parts))

    # if last item is None, drop it (can happen when called with [profile, hat] when hat is None)
    if name_parts[len(name_parts)-1] is None:
        name_parts.pop(-1)

    return '//'.join(name_parts)


class DebugLogger:
    """Unified debug facility. Logs to file or stderr.

    Does not log anything by default. Will only log if environment variable
    LOGPROF_DEBUG is set to a number between 1 and 3 or if method activateStderr
    is run.
    """
    def __init__(self, module_name=__name__):
        self.debugging = False
        self.debug_level = logging.DEBUG

        if os.getenv('LOGPROF_DEBUG', False):
            self.logfile = '/var/log/apparmor/logprof.log'
            self.debugging = os.getenv('LOGPROF_DEBUG')
            try:
                self.debugging = int(self.debugging)
            except Exception:
                self.debugging = False
            if self.debugging not in range(0, 4):
                sys.stdout.write('Environment Variable: LOGPROF_DEBUG contains invalid value: %s'
                                 % os.getenv('LOGPROF_DEBUG'))
            if self.debugging == 0:  # debugging disabled, don't need to setup logging
                return
            if self.debugging == 1:
                self.debug_level = logging.ERROR  # 40
            elif self.debugging == 2:
                self.debug_level = logging.INFO  # 20
            elif self.debugging == 3:
                self.debug_level = logging.DEBUG  # 10

            try:
                logging.basicConfig(filename=self.logfile, level=self.debug_level,
                                    format='%(asctime)s - %(name)s - %(message)s\n')
            except IOError:
                # Unable to open the default logfile, so create a temporary logfile and tell use about it
                templog = NamedTemporaryFile('w', prefix='apparmor', suffix='.log', delete=False)
                templog.close()
                sys.stdout.write("\nCould not open: %s\nLogging to: %s\n" % (self.logfile, templog.name))

                logging.basicConfig(filename=templog.name, level=self.debug_level,
                                    format='%(asctime)s - %(name)s - %(message)s\n')

            self.logger = logging.getLogger(module_name)

    def activateStderr(self):
        self.debugging = True
        logging.basicConfig(
            level=self.debug_level,
            format='%(levelname)s: %(message)s',
            stream=sys.stderr,
        )
        self.logger = logging.getLogger(__name__)

    def error(self, message):
        if self.debugging:
            self.logger.error(message)

    def info(self, message):
        if self.debugging:
            self.logger.info(message)

    def debug(self, message):
        if self.debugging:
            self.logger.debug(message)

    def shutdown(self):
        logging.shutdown()
        # logging.shutdown([self.logger])
