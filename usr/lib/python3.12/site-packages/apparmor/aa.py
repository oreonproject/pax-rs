# ----------------------------------------------------------------------
#    Copyright (C) 2013 Kshitij Gupta <kgupta8592@gmail.com>
#    Copyright (C) 2014-2021 Christian Boltz <apparmor@cboltz.de>
#
#    This program is free software; you can redistribute it and/or
#    modify it under the terms of version 2 of the GNU General Public
#    License as published by the Free Software Foundation.
#
#    This program is distributed in the hope that it will be useful,
#    but WITHOUT ANY WARRANTY; without even the implied warranty of
#    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
#    GNU General Public License for more details.
#
# ----------------------------------------------------------------------
# No old version logs, only 2.6 + supported
import atexit
import os
import re
import shutil
import subprocess  # nosec
import sys
import time
import traceback
from copy import deepcopy
from shutil import which
from tempfile import NamedTemporaryFile

import apparmor.config
import apparmor.logparser
import apparmor.rules as aarules
import apparmor.severity
import apparmor.ui as aaui
from apparmor.aare import AARE
from apparmor.common import (
    AppArmorBug, AppArmorException, DebugLogger, cmd, combine_profname, hasher,
    is_skippable_file, open_file_read, open_file_write, split_name, valid_path)
from apparmor.profile_list import ProfileList, preamble_ruletypes
from apparmor.profile_storage import ProfileStorage, add_or_remove_flag, ruletypes
from apparmor.regex import (
    RE_HAS_COMMENT_SPLIT, RE_PROFILE_CHANGE_HAT, RE_PROFILE_CONDITIONAL,
    RE_PROFILE_CONDITIONAL_BOOLEAN, RE_PROFILE_CONDITIONAL_VARIABLE, RE_PROFILE_END,
    RE_PROFILE_HAT_DEF, RE_PROFILE_MOUNT, RE_PROFILE_PIVOT_ROOT, RE_PROFILE_START,
    RE_PROFILE_UNIX, RE_RULE_HAS_COMMA, parse_profile_start_line, re_match_include)
from apparmor.rule.abi import AbiRule
from apparmor.rule.capability import CapabilityRule
from apparmor.rule.change_profile import ChangeProfileRule
from apparmor.rule.dbus import DbusRule
from apparmor.rule.file import FileRule
from apparmor.rule.include import IncludeRule
from apparmor.rule.network import NetworkRule
from apparmor.rule.ptrace import PtraceRule
from apparmor.rule.signal import SignalRule
from apparmor.translations import init_translation

_ = init_translation()

# Setup logging in case debugging is enabled
debug_logger = DebugLogger('aa')

# The database for severity
sev_db = None
# The file to read log messages from
### Was our
logfile = None

CONFDIR = None
conf = None
cfg = None

parser = None
profile_dir = None
extra_profile_dir = None
### end our
# To keep track of previously included profile fragments
include = dict()

active_profiles = ProfileList()
extra_profiles = ProfileList()

# To store the globs entered by users so they can be provided again
# format: user_globs['/foo*'] = AARE('/foo*')
user_globs = {}

# let ask_addhat() remember answers for already-seen change_hat events
transitions = {}

aa = {}  # Profiles originally in sd, replace by aa
original_aa = hasher()
### end our

changed = dict()
created = []
helpers = dict()  # Preserve this between passes # was our
### logprof ends


def reset_aa():
    """Reset the most important global variables

       Used by aa-mergeprof and some tests.
    """

    global aa, include, active_profiles, original_aa

    aa = {}
    include = dict()
    active_profiles = ProfileList()
    original_aa = hasher()


def on_exit():
    """Shutdowns the logger and records exit if debugging enabled"""
    debug_logger.debug('Exiting..')
    debug_logger.shutdown()


# Register the on_exit method with atexit
atexit.register(on_exit)


def check_for_LD_XXX(file):
    """Returns True if specified program contains references to LD_PRELOAD or
    LD_LIBRARY_PATH to give the Px/Ux code better suggestions"""
    if not os.path.isfile(file):
        return False
    size = os.stat(file).st_size
    # Limit to checking files under 100k for the sake of speed
    if size > 100000:
        return False
    with open(file, 'rb') as f_in:
        for line in f_in:
            if b'LD_PRELOAD' in line or b'LD_LIBRARY_PATH' in line:
                return True
    return False


def fatal_error(message):
    # Get the traceback to the message
    tb_stack = traceback.format_list(traceback.extract_stack())
    tb_stack = ''.join(tb_stack)
    # Add the traceback to message
    message = tb_stack + '\n\n' + message
    debug_logger.error(message)

    # Else tell user what happened
    aaui.UI_Important(message)
    sys.exit(1)


def check_for_apparmor(filesystem='/proc/filesystems', mounts='/proc/mounts'):
    """Finds and returns the mountpoint for apparmor None otherwise"""
    support_securityfs = False
    aa_mountpoint = None
    if valid_path(filesystem):
        with open_file_read(filesystem) as f_in:
            for line in f_in:
                if 'securityfs' in line:
                    support_securityfs = True
                    break
    if valid_path(mounts) and support_securityfs:
        with open_file_read(mounts) as f_in:
            for line in f_in:
                split = line.split()
                if len(split) > 2 and split[2] == 'securityfs':
                    mountpoint = split[1] + '/apparmor'
                    # Check if apparmor is actually mounted there
                    # XXX valid_path() only checks the syntax, but not if the directory exists!
                    if valid_path(mountpoint) and valid_path(mountpoint + '/profiles'):
                        aa_mountpoint = mountpoint
                        break
    return aa_mountpoint


def get_full_path(original_path):
    """Return the full path after resolving any symlinks"""
    path = original_path
    link_count = 0
    if not path.startswith('/'):
        path = os.path.join(os.getcwd(), path)
    while os.path.islink(path):
        link_count += 1
        if link_count > 64:
            fatal_error(_("Followed too many links while resolving %s") % (original_path))
        direc, file = os.path.split(path)
        link = os.readlink(path)
        # If the link an absolute path
        if link.startswith('/'):
            path = link
        else:
            # Link is relative path
            path = os.path.join(direc, link)
    return os.path.realpath(path)


def find_executable(bin_path):
    """Returns the full executable path for the given executable, None otherwise"""
    full_bin = None
    if os.path.exists(bin_path):
        full_bin = get_full_path(bin_path)
    else:
        if '/' not in bin_path:
            env_bin = which(bin_path)
            if env_bin:
                full_bin = get_full_path(env_bin)
    if full_bin and os.path.exists(full_bin):
        return full_bin
    return None


def get_profile_filename_from_profile_name(profile, get_new=False):
    """Returns the full profile name for the given profile name"""

    filename = active_profiles.filename_from_profile_name(profile)
    if filename:
        return filename

    if get_new:
        return get_new_profile_filename(profile)


def get_profile_filename_from_attachment(profile, get_new=False):
    """Returns the full profile name for the given attachment"""

    filename = active_profiles.filename_from_attachment(profile)
    if filename:
        return filename

    if get_new:
        return get_new_profile_filename(profile)


def get_new_profile_filename(profile):
    """Compose filename for a new profile"""
    if profile.startswith('/'):
        # Remove leading /
        filename = profile[1:]
    else:
        filename = "profile_" + profile
    filename = filename.replace('/', '.')
    filename = os.path.join(profile_dir, filename)
    return filename


def name_to_prof_filename(prof_filename):
    """Returns the profile"""
    if prof_filename.startswith(profile_dir):
        profile = prof_filename.split(profile_dir, 1)[1]
        return (prof_filename, profile)
    else:
        bin_path = find_executable(prof_filename)
        if bin_path:
            prof_filename = get_profile_filename_from_attachment(bin_path, True)
            if os.path.isfile(prof_filename):
                return (prof_filename, bin_path)

    return None, None


def complain(path):
    """Sets the profile to complain mode if it exists"""
    prof_filename, name = name_to_prof_filename(path)
    if not prof_filename:
        fatal_error(_("Can't find %s") % path)
    set_complain(prof_filename, name)


def enforce(path):
    """Sets the profile to enforce mode if it exists"""
    prof_filename, name = name_to_prof_filename(path)
    if not prof_filename:
        fatal_error(_("Can't find %s") % path)
    set_enforce(prof_filename, name)


def set_complain(filename, program):
    """Sets the profile to complain mode"""
    aaui.UI_Info(_('Setting %s to complain mode.') % (filename if program is None else program))
    # a force-complain symlink is more packaging-friendly, but breaks caching
    # create_symlink('force-complain', filename)
    delete_symlink('disable', filename)
    change_profile_flags(filename, program, ['enforce', 'kill', 'unconfined', 'prompt'], False)  # remove conflicting mode flags
    change_profile_flags(filename, program, 'complain', True)


def set_enforce(filename, program):
    """Sets the profile to enforce mode"""
    aaui.UI_Info(_('Setting %s to enforce mode.') % (filename if program is None else program))
    delete_symlink('force-complain', filename)
    delete_symlink('disable', filename)
    change_profile_flags(filename, program, ['complain', 'kill', 'unconfined', 'prompt'], False)  # remove conflicting and complain mode flags


def delete_symlink(subdir, filename):
    path = filename
    link = re.sub('^%s' % profile_dir, '%s/%s' % (profile_dir, subdir), path)
    if link != path and os.path.islink(link):
        os.remove(link)


def create_symlink(subdir, filename):
    path = filename
    bname = os.path.basename(filename)
    if not bname:
        raise AppArmorException(_('Unable to find basename for %s.') % filename)
    # print(filename)
    link = re.sub('^%s' % profile_dir, '%s/%s' % (profile_dir, subdir), path)
    # print(link)
    # link = link + '/%s'%bname
    # print(link)
    symlink_dir = os.path.dirname(link)
    if not os.path.exists(symlink_dir):
        # If the symlink directory does not exist create it
        os.makedirs(symlink_dir)

    if not os.path.exists(link):
        try:
            os.symlink(filename, link)
        except:
            raise AppArmorException(
                _('Could not create %(link)s symlink to %(file)s.')
                % {'link': link, 'file': filename})


def head(file):
    """Returns the first/head line of the file"""
    first = ''
    if os.path.isfile(file):
        with open_file_read(file) as f_in:
            try:
                first = f_in.readline().rstrip()
            except UnicodeDecodeError:
                pass
            return first
    else:
        raise AppArmorException(_('Unable to read first line from %s: File Not Found') % file)


def get_output(params):
    """Runs the program with the given args and returns the return code and stdout (as list of lines)"""
    try:
        # Get the output of the program
        output = subprocess.check_output(params)  # nosec
        ret = 0
    except OSError as e:
        raise AppArmorException(
            _("Unable to fork: %(program)s\n\t%(error)s")
            % {'program': params[0], 'error': str(e)})
    except subprocess.CalledProcessError as e:  # If exit code != 0
        output = e.output
        ret = e.returncode

    output = output.decode('utf-8').split('\n')

    # Remove the extra empty string caused due to \n if present
    if not output[-1]:
        output.pop()

    return (ret, output)


def get_reqs(file):
    """Returns a list of paths from ldd output"""
    pattern1 = re.compile(r'^\s*\S+ => (/\S+)')
    pattern2 = re.compile(r'^\s*(/\S+)')
    reqs = []

    ldd = conf.find_first_file(cfg['settings'].get('ldd')) or '/usr/bin/ldd'
    if not os.path.isfile(ldd) or not os.access(ldd, os.EX_OK):
        raise AppArmorException("Can't find ldd")

    ret, ldd_out = get_output((ldd, file))
    if ret == 0 or ret == 1:
        for line in ldd_out:
            if 'not a dynamic executable' in line:  # comes with ret == 1
                break
            if 'cannot read header' in line:
                break
            if 'statically linked' in line:
                break
            match = pattern1.search(line)
            if match:
                reqs.append(match.groups()[0])
            else:
                match = pattern2.search(line)
                if match:
                    reqs.append(match.groups()[0])
    return reqs


def handle_binfmt(profile, path):
    """Modifies the profile to add the requirements"""
    reqs_processed = dict()
    reqs = get_reqs(path)
    while reqs:
        library = reqs.pop()
        library = get_full_path(library)  # resolve symlinks
        if not reqs_processed.get(library, False):
            if get_reqs(library):
                reqs.extend(get_reqs(library))
            reqs_processed[library] = True

        library_rule = FileRule(library, 'mr', None, FileRule.ALL, owner=False, log_event=True)

        if not is_known_rule(profile, 'file', library_rule):
            globbed_library = glob_common(library)
            if globbed_library:
                # glob_common returns a list, just use the first element (typically '/lib/libfoo.so.*')
                library_rule = FileRule(globbed_library[0], 'mr', None, FileRule.ALL, owner=False)

            profile['file'].add(library_rule)


def get_interpreter_and_abstraction(exec_target):
    """Check if exec_target is a script.
       If a hashbang is found, check if we have an abstraction for it.

       Returns (interpreter_path, abstraction)
       - interpreter_path is none if exec_target is not a script or doesn't have a hashbang line
       - abstraction is None if no matching abstraction exists"""

    if not os.path.exists(exec_target):
        aaui.UI_Important(_('Execute target %s does not exist!') % exec_target)
        return None, None

    if not os.path.isfile(exec_target):
        aaui.UI_Important(_('Execute target %s is not a file!') % exec_target)
        return None, None

    hashbang = head(exec_target)
    if not hashbang.startswith('#!'):
        return None, None

    # get the interpreter (without parameters)
    interpreter = hashbang[2:].strip().split()[0]
    interpreter_path = get_full_path(interpreter)
    interpreter = re.sub('^(/usr)?/bin/', '', interpreter_path)

    if interpreter in ('bash', 'dash', 'sh'):
        abstraction = 'abstractions/bash'
    elif interpreter == 'perl':
        abstraction = 'abstractions/perl'
    elif re.search(r'^python([23]|[23]\.[0-9]+)?$', interpreter):
        abstraction = 'abstractions/python'
    elif re.search(r'^ruby([0-9]+(\.[0-9]+)*)?$', interpreter):
        abstraction = 'abstractions/ruby'
    else:
        abstraction = None

    return interpreter_path, abstraction


def create_new_profile(localfile, is_stub=False):
    local_profile = {}
    local_profile[localfile] = ProfileStorage('NEW', localfile, 'create_new_profile()')
    local_profile[localfile]['flags'] = 'complain'

    if os.path.join(profile_dir, 'abstractions/base') in include:
        local_profile[localfile]['inc_ie'].add(IncludeRule('abstractions/base', False, True))
    else:
        aaui.UI_Important(_("WARNING: Can't find %s, therefore not adding it to the new profile.") % 'abstractions/base')

    if os.path.exists(localfile) and os.path.isfile(localfile):
        interpreter_path, abstraction = get_interpreter_and_abstraction(localfile)

        if interpreter_path:
            local_profile[localfile]['file'].add(FileRule(localfile,        'r',  None, FileRule.ALL, owner=False))
            local_profile[localfile]['file'].add(FileRule(interpreter_path, None, 'ix', FileRule.ALL, owner=False))

            if abstraction:
                if os.path.join(profile_dir, abstraction) in include:
                    local_profile[localfile]['inc_ie'].add(IncludeRule(abstraction, False, True))
                else:
                    aaui.UI_Important(_("WARNING: Can't find %s, therefore not adding it to the new profile.") % abstraction)

            handle_binfmt(local_profile[localfile], interpreter_path)
        else:
            local_profile[localfile]['file'].add(FileRule(localfile,        'mr', None, FileRule.ALL, owner=False))

            handle_binfmt(local_profile[localfile], localfile)
    # Add required hats to the profile if they match the localfile
    for hatglob in cfg['required_hats'].keys():
        if re.search(hatglob, localfile):
            for hat in sorted(cfg['required_hats'][hatglob].split()):
                full_hat = combine_profname([localfile, hat])
                if not local_profile.get(full_hat, False):
                    local_profile[full_hat] = ProfileStorage('NEW', hat, 'create_new_profile() required_hats')
                    local_profile[full_hat]['is_hat'] = True
                local_profile[full_hat]['flags'] = 'complain'

    if not is_stub:
        created.append(localfile)
        changed[localfile] = True

    debug_logger.debug("Profile for %s:\n\t%s" % (localfile, local_profile.__str__()))
    return local_profile


def delete_profile(local_prof):
    """Deletes the specified file from the disk and remove it from our list"""
    profile_file = get_profile_filename_from_profile_name(local_prof, True)
    if os.path.isfile(profile_file):
        os.remove(profile_file)
    if aa.get(local_prof, False):
        aa.pop(local_prof)

    # prof_unload(local_prof)


def confirm_and_abort():
    ans = aaui.UI_YesNo(_('Are you sure you want to abandon this set of profile changes and exit?'), 'n')
    if ans == 'y':
        aaui.UI_Info(_('Abandoning all changes.'))
        for prof in created:
            delete_profile(prof)
        sys.exit(0)


def get_profile(prof_name):
    """search for inactive/extra profile, and ask if it should be used"""

    if not extra_profiles.profiles.get(prof_name, False):
        return None  # no inactive profile found

    # TODO: search based on the attachment, not (only?) based on the profile name
    #       (Note: in theory, multiple inactive profiles (with different profile names) could exist for a binary.)
    inactive_profile = deepcopy(extra_profiles.get_profile_and_childs(prof_name))

    orig_filename = inactive_profile[prof_name]['filename']  # needed for CMD_VIEW_PROFILE

    for prof in inactive_profile:
        inactive_profile[prof]['flags'] = 'complain'  # TODO: preserve other flags, if any
        inactive_profile[prof]['filename'] = ''

    # ensure active_profiles has the /etc/apparmor.d/ filename initialized
    # TODO: ideally serialize_profile() shouldn't always use active_profiles
    prof_filename = get_new_profile_filename(prof_name)
    if not active_profiles.files.get(prof_filename):
        active_profiles.init_file(prof_filename)

    uname = 'Inactive local profile for %s' % prof_name
    profile_hash = {
        uname: {
            'profile': serialize_profile(inactive_profile, prof_name, {}),
            'profile_data': inactive_profile,
        }
    }

    options = [uname]

    q = aaui.PromptQuestion()
    q.headers = ['Profile', prof_name]
    q.functions = ['CMD_VIEW_PROFILE', 'CMD_USE_PROFILE', 'CMD_CREATE_PROFILE', 'CMD_ABORT']
    q.default = "CMD_VIEW_PROFILE"
    q.options = options
    q.selected = 0

    ans = ''
    while 'CMD_USE_PROFILE' not in ans and 'CMD_CREATE_PROFILE' not in ans:
        ans, arg = q.promptUser()
        p = profile_hash[options[arg]]
        q.selected = options.index(options[arg])
        if ans == 'CMD_VIEW_PROFILE':
            aaui.UI_ShowFile(uname, orig_filename)
        elif ans == 'CMD_USE_PROFILE':
            created.append(prof_name)
            return p['profile_data']

    return None  # CMD_CREATE_PROFILE chosen


def autodep(bin_name, pname=''):
    bin_full = None
    if bin_name:
        bin_full = find_executable(bin_name)
        # if not bin_full:
        #    bin_full = bin_name
        # if not bin_full.startswith('/'):
        #     return
        # Return if executable path not found
        if not bin_full:
            return
    else:
        bin_full = pname  # for named profiles

    pname = bin_full
    read_inactive_profiles()
    profile_data = get_profile(pname)
    # Create a new profile if no existing profile
    if not profile_data:
        profile_data = create_new_profile(pname)
    file = get_profile_filename_from_profile_name(pname, True)
    profile_data[pname]['filename'] = file  # change filename from extra_profile_dir to /etc/apparmor.d/

    attach_profile_data(aa, profile_data)
    attach_profile_data(original_aa, profile_data)

    attachment = profile_data[pname]['attachment']
    if not attachment and pname.startswith('/'):
        attachment = pname  # use name as name and attachment

    active_profiles.add_profile(file, pname, attachment)

    if os.path.isfile(profile_dir + '/abi/3.0'):
        active_profiles.add_abi(file, AbiRule('abi/3.0', False, True))
    if os.path.isfile(profile_dir + '/tunables/global'):
        active_profiles.add_inc_ie(file, IncludeRule('tunables/global', False, True))
    write_profile_ui_feedback(pname)


def get_profile_flags(filename, program):
    # To-Do
    # XXX If more than one profile in a file then second one is being ignored XXX
    # Do we return flags for both or
    flags = ''
    with open_file_read(filename) as f_in:
        for line in f_in:
            if RE_PROFILE_START.search(line):
                matches = parse_profile_start_line(line, filename)
                if (matches['attachment'] is not None):
                    profile_glob = AARE(matches['attachment'], True)
                else:
                    profile_glob = AARE(matches['profile'], True)
                flags = matches['flags']
                if ((program is not None and profile_glob.match(program))
                        or program is None or program == matches['profile']):
                    return flags

    raise AppArmorException(_('%s contains no profile') % filename)


def change_profile_flags(prof_filename, program, flag, set_flag):
    """Reads the old profile file and updates the flags accordingly"""
    # TODO: count the number of matching lines (separated by profile and hat?) and return it
    #       so that code calling this function can make sure to only report success if there was a match
    # TODO: change child profile flags even if program is specified

    found = False
    depth = -1

    if not flag or (type(flag) is str and not flag.strip()):
        raise AppArmorBug('New flag for %s is empty' % prof_filename)

    with open_file_read(prof_filename) as f_in:
        temp_file = NamedTemporaryFile('w', prefix=prof_filename, suffix='~', delete=False, dir=profile_dir)
        temp_file.close()
        shutil.copymode(prof_filename, temp_file.name)
        with open_file_write(temp_file.name) as f_out:
            for lineno, line in enumerate(f_in):
                if RE_PROFILE_START.search(line):
                    depth += 1
                    (profile, hat, prof_storage) = ProfileStorage.parse(line, prof_filename, lineno, '', '')
                    old_flags = prof_storage['flags']
                    newflags = ', '.join(add_or_remove_flag(old_flags, flag, set_flag))

                    if (prof_storage['attachment']):
                        profile_glob = AARE(prof_storage['attachment'], True)
                    else:
                        profile_glob = AARE(prof_storage['name'], False)  # named profiles can come without an attachment path specified ("profile foo {...}")

                    if (program is not None and profile_glob.match(program)) or program is None or program == prof_storage['name']:
                        found = True
                        if program is not None and program != profile:
                            aaui.UI_Info(_('Warning: profile %s represents multiple programs') % profile)

                        prof_storage['flags'] = newflags

                        line = prof_storage.get_header(depth, profile, False)
                        line = '%s\n' % line[0]
                elif RE_PROFILE_HAT_DEF.search(line):
                    depth += 1
                    (profile, hat, prof_storage) = ProfileStorage.parse(line, prof_filename, lineno, '', '')
                    old_flags = prof_storage['flags']
                    newflags = ', '.join(add_or_remove_flag(old_flags, flag, set_flag))
                    prof_storage['flags'] = newflags

                    line = prof_storage.get_header(depth, profile, False)
                    line = '%s\n' % line[0]
                elif RE_PROFILE_END.search(line):
                    depth -= 1

                f_out.write(line)
    os.rename(temp_file.name, prof_filename)

    if not found:
        if program is None:
            raise AppArmorException("%(file)s doesn't contain a valid profile (syntax error?)" % {'file': prof_filename})
        else:
            raise AppArmorException("%(file)s doesn't contain a valid profile for %(profile)s (syntax error?)" % {'file': prof_filename, 'profile': program})


def profile_exists(program):
    """Returns True if profile exists, False otherwise"""
    # Check cache of profiles

    if active_profiles.filename_from_attachment(program):
        return True
    # Check the disk for profile
    prof_path = get_profile_filename_from_attachment(program, True)
    # print(prof_path)
    if os.path.isfile(prof_path):
        # Add to cache of profile
        raise AppArmorBug('Reached strange condition in profile_exists(), please open a bugreport!')
        # active_profiles[program] = prof_path
        # return True
    return False


def build_x_functions(default, options, exec_toggle):
    ret_list = []
    fallback_toggle = False
    if exec_toggle:
        if 'i' in options:
            ret_list.append('CMD_ix')
            if 'p' in options:
                ret_list.append('CMD_pix')
                fallback_toggle = True
            if 'c' in options:
                ret_list.append('CMD_cix')
                fallback_toggle = True
            if 'n' in options:
                ret_list.append('CMD_nix')
                fallback_toggle = True
            if fallback_toggle:
                ret_list.append('CMD_EXEC_IX_OFF')
        if 'u' in options:
            ret_list.append('CMD_ux')

    else:
        if 'i' in options:
            ret_list.append('CMD_ix')
        if 'c' in options:
            ret_list.append('CMD_cx')
            fallback_toggle = True
        if 'p' in options:
            ret_list.append('CMD_px')
            fallback_toggle = True
        if 'n' in options:
            ret_list.append('CMD_nx')
            fallback_toggle = True
        if 'u' in options:
            ret_list.append('CMD_ux')

        if fallback_toggle:
            ret_list.append('CMD_EXEC_IX_ON')

    ret_list.extend(('CMD_DENY', 'CMD_ABORT', 'CMD_FINISHED'))
    return ret_list


def ask_addhat(hashlog):
    """ask the user about change_hat events (requests to add a hat)"""

    for aamode in hashlog:
        for profile in hashlog[aamode]:
            if '//' in hashlog[aamode][profile]['final_name'] and hashlog[aamode][profile]['change_hat'].keys():
                aaui.UI_Important('Ignoring change_hat event for %s, nested profiles are not supported yet.' % profile)
                continue

            for full_hat in hashlog[aamode][profile]['change_hat']:
                hat = full_hat.split('//')[-1]

                if aa[profile].get(hat, False):
                    continue  # no need to ask if the hat already exists

                default_hat = None
                for hatglob in cfg.options('defaulthat'):
                    if re.search(hatglob, profile):
                        default_hat = cfg['defaulthat'][hatglob]

                context = profile + ' -> ^%s' % hat
                ans = transitions.get(context, 'XXXINVALIDXXX')

                while ans not in ('CMD_ADDHAT', 'CMD_USEDEFAULT', 'CMD_DENY'):
                    q = aaui.PromptQuestion()
                    q.headers.extend((_('Profile'), profile))

                    if default_hat:
                        q.headers.extend((_('Default Hat'), default_hat))

                    q.headers.extend((_('Requested Hat'), hat))

                    q.functions.append('CMD_ADDHAT')
                    if default_hat:
                        q.functions.append('CMD_USEDEFAULT')
                    q.functions.extend(('CMD_DENY', 'CMD_ABORT', 'CMD_FINISHED'))

                    q.default = 'CMD_DENY'
                    if aamode == 'PERMITTING':
                        q.default = 'CMD_ADDHAT'

                    ans = q.promptUser()[0]

                    if ans == 'CMD_FINISHED':
                        save_profiles()
                        return

                transitions[context] = ans

                if ans == 'CMD_ADDHAT':
                    aa[profile][hat] = ProfileStorage(profile, hat, 'ask_addhat addhat')
                    aa[profile][hat]['flags'] = aa[profile][profile]['flags']
                    hashlog[aamode][full_hat]['final_name'] = '%s//%s' % (profile, hat)
                    changed[profile] = True
                elif ans == 'CMD_USEDEFAULT':
                    hat = default_hat
                    hashlog[aamode][full_hat]['final_name'] = '%s//%s' % (profile, default_hat)
                    if not aa[profile].get(hat, False):
                        # create default hat if it doesn't exist yet
                        aa[profile][hat] = ProfileStorage(profile, hat, 'ask_addhat default hat')
                        aa[profile][hat]['flags'] = aa[profile][profile]['flags']
                        changed[profile] = True
                elif ans == 'CMD_DENY':
                    # As unknown hat is denied no entry for it should be made
                    hashlog[aamode][full_hat]['final_name'] = ''
                    continue


def ask_exec(hashlog):
    """ask the user about exec events (requests to execute another program) and which exec mode to use"""

    for aamode in hashlog:
        for full_profile in hashlog[aamode]:
            profile, hat = split_name(full_profile)  # XXX temporary solution to avoid breaking the existing code

            for exec_target in hashlog[aamode][full_profile]['exec']:
                for target_profile in hashlog[aamode][full_profile]['exec'][exec_target]:
                    to_name = ''

                    if os.path.isdir(exec_target):
                        raise AppArmorBug(
                            'exec permissions requested for directory %s (profile %s). This should not happen - please open a bugreport!' % (exec_target, full_profile))

                    if not aa[profile].get(hat):
                        continue  # ignore log entries for non-existing profiles

                    exec_event = FileRule(exec_target, None, FileRule.ANY_EXEC, FileRule.ALL, owner=False, log_event=True)
                    if is_known_rule(aa[profile][hat], 'file', exec_event):
                        continue

                    # nx is not used in profiles but in log files.
                    # Log parsing methods will convert it to its profile form
                    # nx is internally cx/px/cix/pix + to_name
                    exec_mode = False
                    file_perm = None

                    if True:
                        options = cfg['qualifiers'].get(exec_target, 'ipcnu')

                        # If profiled program executes itself only 'ix' option
                        # if exec_target == profile:
                        #     options = 'i'

                        # Don't allow hats to cx?
                        if '//' in hashlog[aamode][full_profile]['final_name'] and hashlog[aamode][full_profile]['exec'].keys():
                            options = options.replace('c', '')

                        # Add deny to options
                        options += 'd'
                        # Define the default option
                        default = None
                        if 'p' in options and os.path.exists(get_profile_filename_from_attachment(exec_target, True)):
                            default = 'CMD_px'
                            sys.stdout.write(_('Target profile exists: %s\n') % get_profile_filename_from_attachment(exec_target, True))
                        elif 'i' in options:
                            default = 'CMD_ix'
                        elif 'c' in options:
                            default = 'CMD_cx'
                        elif 'n' in options:
                            default = 'CMD_nx'
                        else:
                            default = 'DENY'

                        #
                        parent_uses_ld_xxx = check_for_LD_XXX(profile)

                        prof_filename = get_profile_filename_from_profile_name(profile)
                        if prof_filename and active_profiles.files.get(prof_filename):
                            sev_db.set_variables(active_profiles.get_all_merged_variables(
                                                 prof_filename,
                                                 include_list_recursive(active_profiles.files[prof_filename], True)))
                        else:
                            sev_db.set_variables({})

                        severity = sev_db.rank_path(exec_target, 'x')

                        # Prompt portion starts
                        q = aaui.PromptQuestion()

                        q.headers.extend((
                            _('Profile'), combine_name(profile, hat),

                            # to_name should not exist here since, transitioning is already handled
                            _('Execute'), exec_target,
                            _('Severity'), severity,
                        ))

                        exec_toggle = False
                        q.functions.extend(build_x_functions(default, options, exec_toggle))

                        # ask user about the exec mode to use
                        ans = ''
                        while ans not in ('CMD_ix', 'CMD_px', 'CMD_cx', 'CMD_nx', 'CMD_pix', 'CMD_cix', 'CMD_nix', 'CMD_ux', 'CMD_DENY'):  # add '(I)gnore'? (hotkey conflict with '(i)x'!)
                            ans = q.promptUser()[0]

                            if ans.startswith('CMD_EXEC_IX_'):
                                exec_toggle = not exec_toggle
                                q.functions = build_x_functions(default, options, exec_toggle)
                                ans = ''
                                continue

                            if ans == 'CMD_FINISHED':
                                save_profiles()
                                return

                            if ans == 'CMD_nx' or ans == 'CMD_nix':
                                arg = exec_target
                                ynans = 'n'
                                if profile == hat:
                                    ynans = aaui.UI_YesNo(_('Are you specifying a transition to a local profile?'), 'n')
                                if ynans == 'y':
                                    if ans == 'CMD_nx':
                                        ans = 'CMD_cx'
                                    else:
                                        ans = 'CMD_cix'
                                else:
                                    if ans == 'CMD_nx':
                                        ans = 'CMD_px'
                                    else:
                                        ans = 'CMD_pix'

                                to_name = aaui.UI_GetString(_('Enter profile name to transition to: '), arg)

                            if ans == 'CMD_ix':
                                exec_mode = 'ix'
                            elif ans in ('CMD_px', 'CMD_cx', 'CMD_pix', 'CMD_cix'):
                                exec_mode = ans.replace('CMD_', '')
                                px_msg = _(
                                    "Should AppArmor sanitise the environment when\n"
                                    "switching profiles?\n"
                                    "\n"
                                    "Sanitising environment is more secure,\n"
                                    "but some applications depend on the presence\n"
                                    "of LD_PRELOAD or LD_LIBRARY_PATH.")
                                if parent_uses_ld_xxx:
                                    px_msg = _(
                                        "Should AppArmor sanitise the environment when\n"
                                        "switching profiles?\n"
                                        "\n"
                                        "Sanitising environment is more secure,\n"
                                        "but this application appears to be using LD_PRELOAD\n"
                                        "or LD_LIBRARY_PATH and sanitising the environment\n"
                                        "could cause functionality problems.")

                                ynans = aaui.UI_YesNo(px_msg, 'y')
                                if ynans == 'y':
                                    # Disable the unsafe mode
                                    exec_mode = exec_mode.capitalize()
                            elif ans == 'CMD_ux':
                                exec_mode = 'ux'
                                ynans = aaui.UI_YesNo(_(
                                    "Launching processes in an unconfined state is a very\n"
                                    "dangerous operation and can cause serious security holes.\n"
                                    "\n"
                                    "Are you absolutely certain you wish to remove all\n"
                                    "AppArmor protection when executing %s ?") % exec_target, 'n')
                                if ynans == 'y':
                                    ynans = aaui.UI_YesNo(_(
                                        "Should AppArmor sanitise the environment when\n"
                                        "running this program unconfined?\n"
                                        "\n"
                                        "Not sanitising the environment when unconfining\n"
                                        "a program opens up significant security holes\n"
                                        "and should be avoided if at all possible."), 'y')
                                    if ynans == 'y':
                                        # Disable the unsafe mode
                                        exec_mode = exec_mode.capitalize()
                                else:
                                    ans = 'INVALID'

                        if exec_mode and 'i' in exec_mode:
                            # For inherit we need mr
                            file_perm = 'mr'
                        else:
                            if ans == 'CMD_DENY':
                                aa[profile][hat]['file'].add(FileRule(exec_target, None, 'x', FileRule.ALL, owner=False, log_event=True, deny=True))
                                changed[profile] = True
                                if target_profile and hashlog[aamode].get(target_profile):
                                    hashlog[aamode][target_profile]['final_name'] = ''
                                # Skip remaining events if they ask to deny exec
                                continue

                        if ans != 'CMD_DENY':
                            if to_name:
                                rule_to_name = to_name
                            else:
                                rule_to_name = FileRule.ALL

                            aa[profile][hat]['file'].add(FileRule(exec_target, file_perm, exec_mode, rule_to_name, owner=False, log_event=True))

                            changed[profile] = True

                            if 'i' in exec_mode:
                                interpreter_path, abstraction = get_interpreter_and_abstraction(exec_target)

                                if interpreter_path:
                                    exec_target_rule = FileRule(exec_target,      'r',  None, FileRule.ALL, owner=False)
                                    interpreter_rule = FileRule(interpreter_path, None, 'ix', FileRule.ALL, owner=False)

                                    if not is_known_rule(aa[profile][hat], 'file', exec_target_rule):
                                        aa[profile][hat]['file'].add(exec_target_rule)
                                    if not is_known_rule(aa[profile][hat], 'file', interpreter_rule):
                                        aa[profile][hat]['file'].add(interpreter_rule)

                                    if abstraction:
                                        abstraction_rule = IncludeRule(abstraction, False, True)

                                        if not aa[profile][hat]['inc_ie'].is_covered(abstraction_rule):
                                            aa[profile][hat]['inc_ie'].add(abstraction_rule)

                                    handle_binfmt(aa[profile][hat], interpreter_path)

                    # Update tracking info based on kind of change

                    if ans == 'CMD_ix':
                        if target_profile and hashlog[aamode].get(target_profile):
                            hashlog[aamode][target_profile]['final_name'] = profile

                    elif re.search('^CMD_(px|nx|pix|nix)', ans):
                        if to_name:
                            exec_target = to_name

                        if target_profile and hashlog[aamode].get(target_profile):
                            hashlog[aamode][target_profile]['final_name'] = exec_target

                        # Check profile exists for px
                        if exec_target.startswith(('/', '@', '{')):
                              prof_filename = get_profile_filename_from_attachment(exec_target, True)
                        else:  # named exec
                              prof_filename = get_profile_filename_from_profile_name(exec_target, True)

                        if not os.path.exists(prof_filename):
                            ynans = 'y'
                            if 'i' in exec_mode:
                                ynans = aaui.UI_YesNo(_('A profile for %s does not exist.\nDo you want to create one?') % exec_target, 'n')
                            if ynans == 'y':
                                helpers[exec_target] = 'enforce'
                                if to_name:
                                    autodep('', exec_target)
                                else:
                                    autodep(exec_target, '')
                                reload_base(exec_target)
                            else:
                                if target_profile and hashlog[aamode].get(target_profile):
                                    hashlog[aamode][target_profile]['final_name'] = profile  # not creating the target profile effectively results in ix mode

                    elif ans.startswith('CMD_cx') or ans.startswith('CMD_cix'):
                        if to_name:
                            exec_target = to_name

                        if not aa[profile].get(exec_target, False):
                            ynans = 'y'
                            if 'i' in exec_mode:
                                ynans = aaui.UI_YesNo(_('A profile for %s does not exist.\nDo you want to create one?') % exec_target, 'n')
                            if ynans == 'y':
                                if not aa[profile].get(exec_target, False):
                                    stub_profile = merged_to_split(create_new_profile(exec_target, True))
                                    aa[profile][exec_target] = stub_profile[exec_target][exec_target]

                                if profile != exec_target:
                                    aa[profile][exec_target]['flags'] = aa[profile][profile]['flags']

                                aa[profile][exec_target]['flags'] = 'complain'

                                if target_profile and hashlog[aamode].get(target_profile):
                                    hashlog[aamode][target_profile]['final_name'] = '%s//%s' % (profile, exec_target)

                            else:
                                if target_profile and hashlog[aamode].get(target_profile):
                                    hashlog[aamode][target_profile]['final_name'] = profile  # not creating the target profile effectively results in ix mode

                    elif ans.startswith('CMD_ux'):
                        continue


def order_globs(globs, original_path):
    """Returns the globs in sorted order, more specific behind"""
    # To-Do
    # ATM its lexicographic, should be done to allow better matches later

    globs = sorted(globs)

    # make sure the original path is always the last option
    if original_path in globs:
        globs.remove(original_path)
    globs.append(original_path)

    return globs


def ask_the_questions(log_dict):
    for aamode in sorted(log_dict.keys()):
        # Describe the type of changes
        if aamode == 'PERMITTING':
            aaui.UI_Info(_('Complain-mode changes:'))
        elif aamode == 'REJECTING':
            aaui.UI_Info(_('Enforce-mode changes:'))
        elif aamode == 'merge':
            pass  # aa-mergeprof
        else:
            raise AppArmorBug(_('Invalid mode found: %s') % aamode)

        for full_profile in sorted(log_dict[aamode].keys()):
            profile, hat = split_name(full_profile)  # XXX limited to two levels to avoid an Exception on nested child profiles or nested null-*

            # TODO: honor full profile name as soon as child profiles are listed in active_profiles
            prof_filename = get_profile_filename_from_profile_name(profile)
            if prof_filename and active_profiles.files.get(prof_filename):
                sev_db.set_variables(active_profiles.get_all_merged_variables(prof_filename, include_list_recursive(active_profiles.files[prof_filename], True)))
            else:
                sev_db.set_variables({})

            if aa.get(profile):  # only continue/ask if the parent profile exists
                if not aa[profile].get(hat, {}).get('file'):
                    if aamode != 'merge':
                        # Ignore log events for a non-existing profile or child profile. Such events can occur
                        # after deleting a profile or hat manually, or when processing a foreign log.
                        # (Checking for 'file' is a simplified way to check if it's a ProfileStorage.)
                        debug_logger.debug("Ignoring events for non-existing profile %s" % full_profile)
                        continue

                    ans = ''
                    while ans not in ('CMD_ADDHAT', 'CMD_ADDSUBPROFILE', 'CMD_DENY'):
                        q = aaui.PromptQuestion()
                        q.headers.extend((_('Profile'), profile))

                        if log_dict[aamode][full_profile]['is_hat']:
                            q.headers.extend((_('Requested Hat'), hat))
                            q.functions.append('CMD_ADDHAT')
                        else:
                            q.headers.extend((_('Requested Subprofile'), hat))
                            q.functions.append('CMD_ADDSUBPROFILE')

                        q.functions.extend(('CMD_DENY', 'CMD_ABORT', 'CMD_FINISHED'))

                        q.default = 'CMD_DENY'

                        ans = q.promptUser()[0]

                        if ans == 'CMD_FINISHED':
                            return

                    if ans == 'CMD_DENY':
                        continue  # don't ask about individual rules if the user doesn't want the additional subprofile/hat

                    if log_dict[aamode][full_profile]['is_hat']:
                        aa[profile][hat] = ProfileStorage(profile, hat, 'mergeprof ask_the_questions() - missing hat')
                        aa[profile][hat]['is_hat'] = True
                    else:
                        aa[profile][hat] = ProfileStorage(profile, hat, 'mergeprof ask_the_questions() - missing subprofile')
                        aa[profile][hat]['is_hat'] = False

                # check for and ask about conflicting exec modes
                ask_conflict_mode(aa[profile][hat], log_dict[aamode][full_profile])

                prof_changed, end_profiling = ask_rule_questions(
                    log_dict[aamode][full_profile], combine_name(profile, hat),
                    aa[profile][hat], ruletypes)
                if prof_changed:
                    changed[profile] = True
                if end_profiling:
                    return  # end profiling loop


def ask_rule_questions(prof_events, profile_name, the_profile, r_types):
    """ask questions about rules to add to a single profile/hat

       parameter       typical value
       prof_events     log_dict[aamode][full_profile]
       profile_name    profile name (possible profile//hat)
       the_profile     aa[profile][hat] -- will be modified
       r_types         ruletypes

       returns:
       changed         True if the profile was changed
       end_profiling   True if the user wants to end profiling
    """

    changed = False

    for ruletype in r_types:
        for rule_obj in prof_events[ruletype].rules:

            if is_known_rule(the_profile, ruletype, rule_obj):
                continue

            default_option = 1
            options = []
            newincludes = match_includes(the_profile, ruletype, rule_obj)
            q = aaui.PromptQuestion()
            if newincludes:
                options.extend(map(lambda inc: 'include <%s>' % inc, sorted(set(newincludes))))

            if ruletype == 'file' and rule_obj.path:
                options += propose_file_rules(the_profile, rule_obj)
            else:
                options.append(rule_obj.get_clean())

            done = False
            while not done:
                q.options = options
                q.selected = default_option - 1
                q.headers = [_('Profile'), profile_name]
                q.headers.extend(rule_obj.logprof_header())

                # Load variables into sev_db? Not needed/used for capabilities and network rules.
                severity = rule_obj.severity(sev_db)
                if severity != sev_db.NOT_IMPLEMENTED:
                    q.headers.extend((_('Severity'), severity))

                q.functions = available_buttons(rule_obj)

                # In complain mode: events default to allow
                # In enforce mode: events default to deny
                # XXX does this behaviour really make sense, except for "historical reasons"[tm]?
                q.default = 'CMD_DENY'
                if rule_obj.log_event == 'PERMITTING':
                    q.default = 'CMD_ALLOW'

                ans, selected = q.promptUser()
                selection = options[selected]

                if ans == 'CMD_IGNORE_ENTRY':
                    done = True
                    break

                elif ans == 'CMD_FINISHED':
                    return changed, True

                elif ans.startswith('CMD_AUDIT'):
                    if ans == 'CMD_AUDIT_NEW':
                        rule_obj.audit = True
                        rule_obj.raw_rule = None
                    else:
                        rule_obj.audit = False
                        rule_obj.raw_rule = None

                    options = set_options_audit_mode(rule_obj, options)

                elif ans.startswith('CMD_USER_'):
                    if ans == 'CMD_USER_ON':
                        rule_obj.owner = True
                        rule_obj.raw_rule = None
                    else:
                        rule_obj.owner = False
                        rule_obj.raw_rule = None

                    options = set_options_owner_mode(rule_obj, options)

                elif ans == 'CMD_ALLOW':
                    done = True
                    changed = True

                    inc = re_match_include(selection)
                    if inc:
                        deleted = delete_all_duplicates(the_profile, inc, r_types)

                        the_profile['inc_ie'].add(IncludeRule.parse(selection))

                        aaui.UI_Info(_('Adding %s to profile.') % selection)
                        if deleted:
                            aaui.UI_Info(_('Deleted %s previous matching profile entries.') % deleted)

                    else:
                        rule_obj = selection_to_rule_obj(rule_obj, selection)
                        deleted = the_profile[ruletype].add(rule_obj, cleanup=True)

                        aaui.UI_Info(_('Adding %s to profile.') % rule_obj.get_clean())
                        if deleted:
                            aaui.UI_Info(_('Deleted %s previous matching profile entries.') % deleted)

                elif ans == 'CMD_DENY':
                    if re_match_include(selection):
                        aaui.UI_Important("Denying via an include file isn't supported by the AppArmor tools")

                    else:
                        done = True
                        changed = True

                        rule_obj = selection_to_rule_obj(rule_obj, selection)
                        rule_obj.deny = True
                        rule_obj.raw_rule = None  # reset raw rule after manually modifying rule_obj
                        deleted = the_profile[ruletype].add(rule_obj, cleanup=True)
                        aaui.UI_Info(_('Adding %s to profile.') % rule_obj.get_clean())
                        if deleted:
                            aaui.UI_Info(_('Deleted %s previous matching profile entries.') % deleted)

                elif ans == 'CMD_GLOB':
                    if not re_match_include(selection):
                        globbed_rule_obj = selection_to_rule_obj(rule_obj, selection)
                        globbed_rule_obj.glob()
                        options, default_option = add_to_options(options, globbed_rule_obj.get_raw())

                elif ans == 'CMD_GLOBEXT':
                    if not re_match_include(selection):
                        globbed_rule_obj = selection_to_rule_obj(rule_obj, selection)
                        globbed_rule_obj.glob_ext()
                        options, default_option = add_to_options(options, globbed_rule_obj.get_raw())

                elif ans == 'CMD_NEW':
                    if not re_match_include(selection):
                        edit_rule_obj = selection_to_rule_obj(rule_obj, selection)
                        prompt, oldpath = edit_rule_obj.edit_header()

                        newpath = aaui.UI_GetString(prompt, oldpath)
                        if newpath:
                            try:
                                input_matches_path = rule_obj.validate_edit(newpath)  # note that we check against the original rule_obj here, not edit_rule_obj (which might be based on a globbed path)
                            except AppArmorException:
                                aaui.UI_Important(_('The path you entered is invalid (not starting with / or a variable)!'))
                                continue

                            if not input_matches_path:
                                ynprompt = (
                                    _('The specified path does not match this log entry:\n'
                                      '\n'
                                      '  Log Entry: %(path)s\n'
                                      '  Entered Path:  %(ans)s\n'
                                      'Do you really want to use this path?')
                                    % {'path': oldpath, 'ans': newpath})
                                key = aaui.UI_YesNo(ynprompt, 'n')
                                if key == 'n':
                                    continue

                            edit_rule_obj.store_edit(newpath)
                            options, default_option = add_to_options(options, edit_rule_obj.get_raw())
                            user_globs[newpath] = AARE(newpath, True)

                else:
                    done = False

    return changed, False


def selection_to_rule_obj(rule_obj, selection):
    rule_type = type(rule_obj)
    return rule_type.parse(selection)


def set_options_audit_mode(rule_obj, options):
    """change audit state in options (proposed rules) to audit state in rule_obj.
       #include options will be kept unchanged
    """
    return set_options_mode(rule_obj, options, 'audit')


def set_options_owner_mode(rule_obj, options):
    """change owner state in options (proposed rules) to owner state in rule_obj.
       #include options will be kept unchanged
    """
    return set_options_mode(rule_obj, options, 'owner')


def set_options_mode(rule_obj, options, what):
    """helper function for set_options_audit_mode() and set_options_owner_mode"""
    new_options = []

    for rule in options:
        if re_match_include(rule):
            new_options.append(rule)
        else:
            parsed_rule = selection_to_rule_obj(rule_obj, rule)
            if what == 'audit':
                parsed_rule.audit = rule_obj.audit
            elif what == 'owner':
                parsed_rule.owner = rule_obj.owner
            else:
                raise AppArmorBug('Unknown "what" value given to set_options_mode: %s' % what)

            parsed_rule.raw_rule = None
            new_options.append(parsed_rule.get_raw())

    return new_options


def available_buttons(rule_obj):
    buttons = []

    if not rule_obj.deny:
        buttons.append('CMD_ALLOW')

    buttons.extend(('CMD_DENY', 'CMD_IGNORE_ENTRY'))

    if rule_obj.can_glob:
        buttons.append('CMD_GLOB')

    if rule_obj.can_glob_ext:
        buttons.append('CMD_GLOBEXT')

    if rule_obj.can_edit:
        buttons.append('CMD_NEW')

    if rule_obj.audit:
        buttons.append('CMD_AUDIT_OFF')
    else:
        buttons.append('CMD_AUDIT_NEW')

    if rule_obj.can_owner:
        if rule_obj.owner:
            buttons.append('CMD_USER_OFF')
        else:
            buttons.append('CMD_USER_ON')

    buttons.extend(('CMD_ABORT', 'CMD_FINISHED'))

    return buttons


def add_to_options(options, newpath):
    if newpath not in options:
        options.append(newpath)

    default_option = options.index(newpath) + 1
    return (options, default_option)


def delete_all_duplicates(profile, incname, r_types):
    deleted = 0
    # Allow rules covered by denied rules shouldn't be deleted
    # only a subset allow rules may actually be denied

    if include.get(incname, False):
        for rule_type in r_types:
            deleted += profile[rule_type].delete_duplicates(include[incname][incname][rule_type])

    return deleted


def ask_conflict_mode(old_profile, merge_profile):
    """ask user about conflicting exec rules"""
    for oldrule in old_profile['file'].rules:
        conflictingrules = merge_profile['file'].get_exec_conflict_rules(oldrule)

        if conflictingrules.rules:
            q = aaui.PromptQuestion()
            q.headers = [
                _('Path'), oldrule.path.regex,
                _('Select the appropriate mode'), '',
            ]
            options = [oldrule.get_clean()]
            for rule in conflictingrules.rules:
                options.append(rule.get_clean())
            q.options = options
            q.functions = ['CMD_ALLOW', 'CMD_ABORT']
            done = False
            while not done:
                ans, selected = q.promptUser()
                if ans == 'CMD_ALLOW':
                    if selected == 0:
                        pass  # just keep the existing rule
                    elif selected > 0:
                        # replace existing rule with merged one
                        old_profile['file'].delete(oldrule)
                        old_profile['file'].add(conflictingrules.rules[selected - 1])
                    else:
                        raise AppArmorException(_('Unknown selection'))

                    for rule in conflictingrules.rules:
                        merge_profile['file'].delete(rule)  # make sure aa-mergeprof doesn't ask to add conflicting rules later

                    done = True


def match_includes(profile, rule_type, rule_obj):
    """propose abstractions that allow the given rule_obj

       Note: This function will return relative paths for includes inside profile_dir
    """

    newincludes = []
    for incname in include.keys():
        rel_incname = incname.replace(profile_dir + '/', '')

        # TODO: improve/fix logic to honor magic vs. quoted include paths
        if rel_incname.startswith('/'):
            is_magic = False
        else:
            is_magic = True

        # never propose includes that are already in the profile (shouldn't happen because of is_known_rule())
        if profile and profile['inc_ie'].is_covered(IncludeRule(rel_incname, False, is_magic)):
            continue

        # never propose a local/ include (they are meant to be included in exactly one profile)
        if rel_incname.startswith('local/'):
            continue

        # XXX type check should go away once we init all profiles correctly
        if valid_include(incname) and include[incname][incname][rule_type].is_covered(rule_obj):
            if include[incname][incname]['logprof_suggest'] != 'no':
                newincludes.append(rel_incname)

    return newincludes


def valid_include(incname):
    """check if the given include file exists or is whitelisted in custom_includes"""
    if cfg['settings']['custom_includes']:
        for incm in cfg['settings']['custom_includes'].split():
            if incm == incname:
                return True

    if incname.startswith('abstractions/') and os.path.isfile(os.path.join(profile_dir, incname)):
        return True
    elif incname.startswith('/') and os.path.isfile(incname):
        return True

    return False


def set_logfile(filename):
    """set logfile to a) the specified filename or b) if not given, the first existing logfile from logprof.conf"""

    global logfile

    if filename:
        logfile = filename
    elif 'logfiles' in cfg['settings']:
        # This line can only run if the 'logfile' exists in settings, otherwise
        # it will yield a Python KeyError
        logfile = conf.find_first_file(cfg['settings']['logfiles']) or '/var/log/syslog'
    else:
        logfile = '/var/log/syslog'

    if not os.path.exists(logfile):
        if filename:
            raise AppArmorException(_('The logfile %s does not exist. Please check the path.') % logfile)
        else:
            raise AppArmorException('Can\'t find system log "%s". Please check permissions.' % (logfile))
    elif os.path.isdir(logfile):
        raise AppArmorException(_('%s is a directory. Please specify a file as logfile') % logfile)


def do_logprof_pass(logmark=''):
    # set up variables for this pass
    global active_profiles
    global sev_db
#    aa = hasher()
#     changed = dict()

    aaui.UI_Info(_('Reading log entries from %s.') % logfile)

    if not sev_db:
        sev_db = apparmor.severity.Severity(CONFDIR + '/severity.db', _('unknown'))
    # print(pid)
    # print(active_profiles)

    log_reader = apparmor.logparser.ReadLog(logfile, active_profiles, profile_dir)
    hashlog = log_reader.read_log(logmark)

    ask_exec(hashlog)
    ask_addhat(hashlog)

    log_dict = collapse_log(hashlog)

    ask_the_questions(log_dict)

    save_profiles()


def save_profiles(is_mergeprof=False):
    # Ensure the changed profiles are actual active profiles
    for prof_name in changed.keys():
        if not aa.get(prof_name, False):
            print("*** save_profiles(): removing %s" % prof_name)
            print('*** This should not happen. Please open a bugreport!')
            changed.pop(prof_name)

    changed_list = sorted(changed.keys())

    if changed_list:
        q = aaui.PromptQuestion()
        q.title = 'Changed Local Profiles'
        q.explanation = _('The following local profiles were changed. Would you like to save them?')
        q.functions = ['CMD_SAVE_CHANGES', 'CMD_SAVE_SELECTED', 'CMD_VIEW_CHANGES', 'CMD_VIEW_CHANGES_CLEAN', 'CMD_ABORT']
        if is_mergeprof:
            q.functions = ['CMD_SAVE_CHANGES', 'CMD_VIEW_CHANGES', 'CMD_ABORT', 'CMD_IGNORE_ENTRY']
        q.default = 'CMD_VIEW_CHANGES'
        q.selected = 0
        ans = ''
        arg = None
        while ans != 'CMD_SAVE_CHANGES':
            if not changed:
                return

            options = sorted(changed.keys())
            q.options = options

            ans, arg = q.promptUser()

            q.selected = arg  # remember selection
            profile_name = options[arg]

            if ans == 'CMD_SAVE_SELECTED':
                write_profile_ui_feedback(profile_name)
                reload_base(profile_name)
                q.selected = 0  # saving the selected profile removes it from the list, therefore reset selection

            elif ans == 'CMD_VIEW_CHANGES':
                oldprofile = None
                if aa[profile_name][profile_name].get('filename', False):
                    oldprofile = aa[profile_name][profile_name]['filename']
                else:
                    oldprofile = get_profile_filename_from_attachment(profile_name, True)

                serialize_options = {'METADATA': True}
                newprofile = serialize_profile(split_to_merged(aa), profile_name, serialize_options)

                aaui.UI_Changes(oldprofile, newprofile, comments=True)

            elif ans == 'CMD_VIEW_CHANGES_CLEAN':
                oldprofile = serialize_profile(split_to_merged(original_aa), profile_name, {})
                newprofile = serialize_profile(split_to_merged(aa), profile_name, {})

                aaui.UI_Changes(oldprofile, newprofile)

            elif ans == 'CMD_IGNORE_ENTRY':
                changed.pop(options[arg])

        for profile_name in sorted(changed.keys()):
            write_profile_ui_feedback(profile_name)
            reload_base(profile_name)


def collapse_log(hashlog, ignore_null_profiles=True):
    log_dict = {}

    for aamode in hashlog.keys():
        log_dict[aamode] = {}

        for full_profile in hashlog[aamode].keys():
            final_name = hashlog[aamode][full_profile]['final_name']

            if final_name == '':
                continue  # user chose "deny" or "unconfined" for this target, therefore ignore log events

            if '//null-' in final_name and ignore_null_profiles:
                # ignore null-* profiles (probably nested childs)
                # otherwise we'd accidentally create a null-* hat in the profile which is worse
                # XXX drop this once we support nested childs
                continue

            profile, hat = split_name(final_name)  # XXX limited to two levels to avoid an Exception on nested child profiles or nested null-*
            # TODO: support nested child profiles

            # used to avoid to accidentally initialize aa[profile][hat] or calling is_known_rule() on events for a non-existing profile
            hat_exists = False
            if aa.get(profile) and aa[profile].get(hat):
                hat_exists = True

            if True:
                if not log_dict[aamode].get(final_name):
                    # with execs in ix mode, we already have ProfileStorage initialized and should keep the content it already has
                    log_dict[aamode][final_name] = ProfileStorage(profile, hat, 'collapse_log()')

                for path in hashlog[aamode][full_profile]['path'].keys():
                    for owner in hashlog[aamode][full_profile]['path'][path]:
                        mode = set(hashlog[aamode][full_profile]['path'][path][owner].keys())

                        # logparser sums up multiple log events, so both 'a' and 'w' can be present
                        if 'a' in mode and 'w' in mode:
                            mode.remove('a')

                        file_event = FileRule(path, mode, None, FileRule.ALL, owner=owner, log_event=True)

                        if not hat_exists or not is_known_rule(aa[profile][hat], 'file', file_event):
                            log_dict[aamode][final_name]['file'].add(file_event)
                            # TODO: check for existing rules with this path, and merge them into one rule

                for cap in hashlog[aamode][full_profile]['capability'].keys():
                    cap_event = CapabilityRule(cap, log_event=True)
                    if not hat_exists or not is_known_rule(aa[profile][hat], 'capability', cap_event):
                        log_dict[aamode][final_name]['capability'].add(cap_event)

                for cp in hashlog[aamode][full_profile]['change_profile'].keys():
                    cp_event = ChangeProfileRule(None, ChangeProfileRule.ALL, cp, log_event=True)
                    if not hat_exists or not is_known_rule(aa[profile][hat], 'change_profile', cp_event):
                        log_dict[aamode][final_name]['change_profile'].add(cp_event)

                dbus = hashlog[aamode][full_profile]['dbus']
                for access in                               dbus:                                      # noqa: E271
                    for bus in                              dbus[access]:                              # noqa: E271
                        for path in                         dbus[access][bus]:                         # noqa: E271
                            for name in                     dbus[access][bus][path]:                   # noqa: E271
                                for interface in            dbus[access][bus][path][name]:             # noqa: E271
                                    for member in           dbus[access][bus][path][name][interface]:  # noqa: E271
                                        for peer_profile in dbus[access][bus][path][name][interface][member]:
                                            # Depending on the access type, not all parameters are allowed.
                                            # Ignore them, even if some of them appear in the log.
                                            # Also, the log doesn't provide a peer name, therefore always use ALL.
                                            if access in ('send', 'receive'):
                                                dbus_event = DbusRule(access, bus, path,         DbusRule.ALL, interface,    member,       DbusRule.ALL, peer_profile, log_event=True)
                                            elif access == 'bind':
                                                dbus_event = DbusRule(access, bus, DbusRule.ALL, name,         DbusRule.ALL, DbusRule.ALL, DbusRule.ALL, DbusRule.ALL, log_event=True)
                                            elif access == 'eavesdrop':
                                                dbus_event = DbusRule(access, bus, DbusRule.ALL, DbusRule.ALL, DbusRule.ALL, DbusRule.ALL, DbusRule.ALL, DbusRule.ALL, log_event=True)
                                            else:
                                                raise AppArmorBug('unexpected dbus access: {}'.format(access))

                                            if not hat_exists or not is_known_rule(aa[profile][hat], 'dbus', dbus_event):
                                                log_dict[aamode][final_name]['dbus'].add(dbus_event)

                nd = hashlog[aamode][full_profile]['network']
                for family in nd.keys():
                    for sock_type in nd[family].keys():
                        net_event = NetworkRule(family, sock_type, log_event=True)
                        if not hat_exists or not is_known_rule(aa[profile][hat], 'network', net_event):
                            log_dict[aamode][final_name]['network'].add(net_event)

                ptrace = hashlog[aamode][full_profile]['ptrace']
                for peer in ptrace.keys():
                    if '//null-' in peer:
                        continue  # ignore null-* peers

                    for access in ptrace[peer].keys():
                        ptrace_event = PtraceRule(access, peer, log_event=True)
                        if not hat_exists or not is_known_rule(aa[profile][hat], 'ptrace', ptrace_event):
                            log_dict[aamode][final_name]['ptrace'].add(ptrace_event)

                sig = hashlog[aamode][full_profile]['signal']
                for peer in sig.keys():
                    if '//null-' in peer:
                        continue  # ignore null-* peers

                    for access in sig[peer].keys():
                        for signal in sig[peer][access].keys():
                            signal_event = SignalRule(access, signal, peer, log_event=True)
                            if not hat_exists or not is_known_rule(aa[profile][hat], 'signal', signal_event):
                                log_dict[aamode][final_name]['signal'].add(signal_event)

    return log_dict


def read_profiles(ui_msg=False, skip_profiles=()):
    # we'll read all profiles from disk, so reset the storage first (autodep() might have created/stored
    # a profile already, which would cause a 'Conflicting profile' error in attach_profile_data())
    #
    # The skip_profiles parameter should only be specified by tests.

    global aa, original_aa
    aa = {}
    original_aa = hasher()

    if ui_msg:
        aaui.UI_Info(_('Updating AppArmor profiles in %s.') % profile_dir)

    try:
        os.listdir(profile_dir)
    except:
        fatal_error(_("Can't read AppArmor profiles in %s") % profile_dir)

    for file in os.listdir(profile_dir):
        full_file = os.path.join(profile_dir, file)
        if os.path.isfile(full_file):
            if is_skippable_file(file):
                continue
            elif file in skip_profiles:
                aaui.UI_Info("skipping profile %s" % full_file)
                continue
            else:
                read_profile(full_file, True)


def read_inactive_profiles(skip_profiles=()):
    # The skip_profiles parameter should only be specified by tests.

    if hasattr(read_inactive_profiles, 'already_read'):
        # each autodep() run calls read_inactive_profiles, but that's a) superfluous and b) triggers a conflict because the inactive profiles are already loaded
        # therefore don't do anything if the inactive profiles were already loaded
        return

    read_inactive_profiles.already_read = True

    if not os.path.exists(extra_profile_dir):
        return
    try:
        os.listdir(profile_dir)
    except:
        fatal_error(_("Can't read AppArmor profiles in %s") % extra_profile_dir)

    for file in os.listdir(extra_profile_dir):
        full_file = os.path.join(extra_profile_dir, file)
        if os.path.isfile(full_file):
            if is_skippable_file(file):
                continue
            elif file in skip_profiles:
                aaui.UI_Info("skipping profile %s" % full_file)
                continue
            else:
                read_profile(full_file, False)


def read_profile(file, active_profile):
    data = None
    try:
        with open_file_read(file) as f_in:
            data = f_in.readlines()
    except IOError as e:
        aaui.UI_Important('WARNING: Error reading file %s, skipping.\n    %s' % (file, e))
        debug_logger.debug("read_profile: can't read %s - skipping" % file)
        return

    profile_data = parse_profile_data(data, file, 0, True)

    if not profile_data:
        return

    if active_profile:
        attach_profile_data(aa, profile_data)
        attach_profile_data(original_aa, profile_data)

        for profile in profile_data:
            if '//' in profile:
                continue  # TODO: handle hats/child profiles independent of main profiles

            attachment = profile_data[profile]['attachment']
            filename = profile_data[profile]['filename']

            if not attachment and profile.startswith('/'):
                attachment = profile  # use profile as name and attachment

            active_profiles.add_profile(filename, profile, attachment)

    else:
        for profile in profile_data:
            attachment = profile_data[profile]['attachment']
            filename = profile_data[profile]['filename']

            if not attachment and profile.startswith('/'):
                attachment = profile  # use profile as name and attachment

            extra_profiles.add_profile(filename, profile, attachment, profile_data[profile])


def attach_profile_data(profiles, profile_data):
    profile_data = merged_to_split(profile_data)
    # Make deep copy of data to avoid changes to
    # arising due to mutables
    for p in profile_data.keys():
        if profiles.get(p, False):
            for hat in profile_data[p].keys():
                if profiles[p].get(hat, False):
                    raise AppArmorException(
                        _("Conflicting profiles for %s defined in two files:\n- %s\n- %s")
                        % (combine_name(p, hat), profiles[p][hat]['filename'], profile_data[p][hat]['filename']))

        profiles[p] = deepcopy(profile_data[p])


def parse_profile_data(data, file, do_include, in_preamble):
    profile_data = {}
    profile = None
    hat = None
    profname = None
    in_contained_hat = None
    parsed_profiles = []
    initial_comment = ''
    lastline = None

    active_profiles.init_file(file)

    if do_include:
        profile = file
        hat = None
        profname = combine_profname([profile, hat])
        profile_data[profname] = ProfileStorage(profile, hat, 'parse_profile_data() do_include')
        profile_data[profname]['filename'] = file

    for lineno, line in enumerate(data):
        line = line.strip()
        if not line:
            continue
        # we're dealing with a multiline statement
        if lastline:
            line = '%s %s' % (lastline, line)
            lastline = None

        # is line handled by a *Rule class?
        (rule_name, rule_obj) = match_line_against_rule_classes(line, profile, file, lineno, in_preamble)
        if rule_name:
            if in_preamble:
                active_profiles.add_rule(file, rule_name, rule_obj)
            else:
                profile_data[profname][rule_name].add(rule_obj)

            if rule_name == 'inc_ie':
                for incname in rule_obj.get_full_paths(profile_dir):
                    if incname == file:
                        # warn about endless loop, and don't call load_include() (again) for this file
                        aaui.UI_Important(_('WARNING: endless loop detected: file %s includes itsself' % incname))
                    else:
                        load_include(incname, in_preamble)

        elif RE_PROFILE_START.search(line) or RE_PROFILE_HAT_DEF.search(line):  # Starting line of a profile/hat
            # in_contained_hat is needed to know if we are already in a profile or not. Simply checking if we are in a hat doesn't work,
            # because something like "profile foo//bar" will set profile and hat at once, and later (wrongfully) expect another "}".
            # The logic is simple and resembles a "poor man's stack" (with limited/hardcoded height).
            if profile:
                in_contained_hat = True
            else:
                in_contained_hat = False

            in_preamble = False

            (profile, hat, prof_storage) = ProfileStorage.parse(line, file, lineno, profile, hat)

            if profile == hat:
                hat = None
            profname = combine_profname([profile, hat])

            if profile_data.get(profname, False):
                raise AppArmorException(
                    'Profile %(profile)s defined twice in %(file)s, last found in line %(line)s'
                    % {'file': file, 'line': lineno + 1, 'profile': combine_name(profile, hat)})

            profile_data[profname] = prof_storage

            # Save the initial comment
            if initial_comment:
                profile_data[profname]['initial_comment'] = initial_comment

            initial_comment = ''

        elif RE_PROFILE_END.search(line):
            # If profile ends and we're not in one
            if not profile:
                raise AppArmorException(
                    _('Syntax Error: Unexpected End of Profile reached in file: %(file)s line: %(line)s')
                    % {'file': file, 'line': lineno + 1})

            if in_contained_hat:
                hat = None
                in_contained_hat = False
                profname = combine_profname([profile, hat])
            else:
                parsed_profiles.append(profile)
                profile = None
                profname = None
                in_preamble = True

            initial_comment = ''

        elif RE_PROFILE_CONDITIONAL.search(line):
            # Conditional Boolean
            pass

        elif RE_PROFILE_CONDITIONAL_VARIABLE.search(line):
            # Conditional Variable defines
            pass

        elif RE_PROFILE_CONDITIONAL_BOOLEAN.search(line):
            # Conditional Boolean defined
            pass

        elif RE_PROFILE_MOUNT.search(line):
            matches = RE_PROFILE_MOUNT.search(line).groups()

            if not profile:
                raise AppArmorException(_('Syntax Error: Unexpected mount entry found in file: %(file)s line: %(line)s')
                                        % {'file': file, 'line': lineno + 1})

            audit = False
            if matches[0]:
                audit = True
            allow = 'allow'
            if matches[1] and matches[1].strip() == 'deny':
                allow = 'deny'
            mount = matches[2]

            mount_rule = parse_mount_rule(mount)
            mount_rule.audit = audit
            mount_rule.deny = (allow == 'deny')

            mount_rules = profile_data[profname][allow].get('mount', [])
            mount_rules.append(mount_rule)
            profile_data[profname][allow]['mount'] = mount_rules

        elif RE_PROFILE_PIVOT_ROOT.search(line):
            matches = RE_PROFILE_PIVOT_ROOT.search(line).groups()

            if not profile:
                raise AppArmorException(_('Syntax Error: Unexpected pivot_root entry found in file: %(file)s line: %(line)s')
                                        % {'file': file, 'line': lineno + 1})

            audit = False
            if matches[0]:
                audit = True
            allow = 'allow'
            if matches[1] and matches[1].strip() == 'deny':
                allow = 'deny'
            pivot_root = matches[2].strip()

            pivot_root_rule = parse_pivot_root_rule(pivot_root)
            pivot_root_rule.audit = audit
            pivot_root_rule.deny = (allow == 'deny')

            pivot_root_rules = profile_data[profname][allow].get('pivot_root', [])
            pivot_root_rules.append(pivot_root_rule)
            profile_data[profname][allow]['pivot_root'] = pivot_root_rules

        elif RE_PROFILE_UNIX.search(line):
            matches = RE_PROFILE_UNIX.search(line).groups()

            if not profile:
                raise AppArmorException(_('Syntax Error: Unexpected unix entry found in file: %(file)s line: %(line)s')
                                        % {'file': file, 'line': lineno + 1})

            audit = False
            if matches[0]:
                audit = True
            allow = 'allow'
            if matches[1] and matches[1].strip() == 'deny':
                allow = 'deny'
            unix = matches[2].strip()

            unix_rule = parse_unix_rule(unix)
            unix_rule.audit = audit
            unix_rule.deny = (allow == 'deny')

            unix_rules = profile_data[profname][allow].get('unix', [])
            unix_rules.append(unix_rule)
            profile_data[profname][allow]['unix'] = unix_rules

        elif RE_PROFILE_CHANGE_HAT.search(line):
            matches = RE_PROFILE_CHANGE_HAT.search(line).groups()

            if not profile:
                raise AppArmorException(_('Syntax Error: Unexpected change hat declaration found in file: %(file)s line: %(line)s')
                                        % {'file': file, 'line': lineno + 1})

            aaui.UI_Important(_('Ignoring no longer supported change hat declaration "^%(hat)s," found in file: %(file)s line: %(line)s')
                              % {'hat': matches[0], 'file': file, 'line': lineno + 1})

        elif line.startswith('#'):
            # Handle initial comments
            if not profile:
                if line.startswith('# Last Modified:'):
                    continue
                else:
                    initial_comment = initial_comment + line + '\n'

            if line.startswith('# LOGPROF-SUGGEST:'):  # TODO: allow any number of spaces/tabs after '#'
                parts = line.split()
                if len(parts) > 2:
                    profile_data[profname]['logprof_suggest'] = parts[2]

                # keep line as part of initial_comment (if we ever support writing abstractions, we should update serialize_profile())
                initial_comment = initial_comment + line + '\n'

        elif not RE_RULE_HAS_COMMA.search(line):
            # Bah, line continues on to the next line
            if RE_HAS_COMMENT_SPLIT.search(line):
                # filter trailing comments
                lastline = RE_HAS_COMMENT_SPLIT.search(line).group('not_comment')
            else:
                lastline = line
        else:
            raise AppArmorException(
                _('Syntax Error: Unknown line found in file %(file)s line %(lineno)s:\n    %(line)s')
                % {'file': file, 'lineno': lineno + 1, 'line': line})

    if lastline:
        # lastline gets merged into line (and reset to None) when reading the next line.
        # If it isn't empty, this means there's something unparsable at the end of the profile
        raise AppArmorException(
            _('Syntax Error: Unknown line found in file %(file)s line %(lineno)s:\n    %(line)s')
            % {'file': file, 'lineno': lineno + 1, 'line': lastline})

    # Below is not required I'd say
    if not do_include:
        for hatglob in cfg['required_hats'].keys():
            for parsed_prof in sorted(parsed_profiles):
                if re.search(hatglob, parsed_prof):
                    for hat in cfg['required_hats'][hatglob].split():
                        profname = combine_profname([parsed_prof, hat])
                        if not profile_data.get(profname, False):
                            profile_data[profname] = ProfileStorage(parsed_prof, hat, 'parse_profile_data() required_hats')
                            profile_data[profname]['is_hat'] = True

    # End of file reached but we're stuck in a profile
    if profile and not do_include:
        raise AppArmorException(
            _("Syntax Error: Missing '}' or ','. Reached end of file %(file)s while inside profile %(profile)s")
            % {'file': file, 'profile': profile})

    return profile_data


def match_line_against_rule_classes(line, profile, file, lineno, in_preamble):
    """handle all lines handled by *Rule classes"""

    for rule_name in (
            'abi',
            'alias',
            'boolean',
            'variable',
            'inc_ie',
            'capability',
            'change_profile',
            'dbus',
            'file',  # file rules need to be parsed after variable rules
            'network',
            'ptrace',
            'rlimit',
            'signal',
    ):

        if rule_name in ruletypes:
            rule_class = ruletypes[rule_name]['rule']
        else:
            rule_class = preamble_ruletypes[rule_name]['rule']

        if rule_class.match(line):
            if not in_preamble and rule_name not in ruletypes:
                raise AppArmorException(
                    _('Syntax Error: Unexpected %(rule)s definition found inside profile in file: %(file)s line: %(line)s')
                    % {'file': file, 'line': lineno + 1, 'rule': rule_name})

            if in_preamble and rule_name not in preamble_ruletypes:
                raise AppArmorException(
                    _('Syntax Error: Unexpected %(rule)s entry found in file: %(file)s line: %(line)s')
                    % {'file': file, 'line': lineno + 1, 'rule': rule_name})

            rule_obj = rule_class.parse(line)
            return (rule_name, rule_obj)

    return (None, None)


def merged_to_split(profile_data):
    """(temporary) helper function to convert a list of profile['foo//bar'] profiles into compat['foo']['bar']"""
    compat = hasher()
    for prof in profile_data:
        profile, hat = split_name(prof)  # XXX limited to two levels to avoid an Exception on nested child profiles or nested null-*
        compat[profile][hat] = profile_data[prof]

    return compat


def split_to_merged(profile_data):
    """(temporary) helper function to convert a traditional compat['foo']['bar'] to a profile['foo//bar'] list"""

    merged = {}

    for profile in profile_data:
        for hat in profile_data[profile]:
            if profile == hat:
                merged_name = profile
            else:
                merged_name = combine_profname([profile, hat])

            merged[merged_name] = profile_data[profile][hat]

    return merged


def parse_mount_rule(line):
    # XXX Do real parsing here
    return aarules.Raw_Mount_Rule(line)


def parse_pivot_root_rule(line):
    # XXX Do real parsing here
    return aarules.Raw_Pivot_Root_Rule(line)


def parse_unix_rule(line):
    # XXX Do real parsing here
    return aarules.Raw_Unix_Rule(line)


def write_piece(profile_data, depth, name, nhat):
    pre = '  ' * depth
    data = []
    wname = None
    inhat = False
    if name == nhat:
        wname = name
    else:
        wname = name + '//' + nhat
        name = nhat
        inhat = True
    data += profile_data[name].get_header(depth, wname, False)
    data += profile_data[name].get_rules_clean(depth + 1)

    pre2 = '  ' * (depth + 1)

    if not inhat:
        # Embedded hats
        all_childs = []
        for child in sorted(profile_data.keys()):
            if child.startswith('%s//' % name):
                all_childs.append(child)

        for hat in all_childs:
            profile, only_hat = split_name(hat)

            if not profile_data[hat]['external']:
                data.append('')

                data += profile_data[hat].get_header(depth + 1, only_hat, True)

                data += profile_data[hat].get_rules_clean(depth + 2)

                data.append('%s}' % pre2)

        data.append('%s}' % pre)

        # External hats
        for hat in all_childs:
            if name == nhat and profile_data[hat].get('external', False):
                data.append('')
                data.extend(map(lambda x: '  %s' % x, write_piece(profile_data, depth - 1, name, nhat)))
                data.append('  }')

    return data


def serialize_profile(profile_data, name, options):
    string = ''
    data = []

    if type(options) is not dict:
        raise AppArmorBug('serialize_profile(): options is not a dict: %s' % options)

    include_metadata = options.get('METADATA', False)

    if include_metadata:
        string = '# Last Modified: %s\n' % time.asctime()

#     if profile_data[name].get('initial_comment', False):
#         comment = profile_data[name]['initial_comment']
#         comment.replace('\\n', '\n')
#         string += comment + '\n'

    if options.get('is_attachment'):
        prof_filename = get_profile_filename_from_attachment(name, True)
    else:
        prof_filename = get_profile_filename_from_profile_name(name, True)

    data.extend(active_profiles.get_clean(prof_filename, 0))

    # Here should be all the profiles from the files added write after global/common stuff
    for prof in sorted(active_profiles.profiles_in_file(prof_filename)):
        if prof != name:
            if original_aa[prof][prof].get('initial_comment', False):
                comment = original_aa[prof][prof]['initial_comment']
                comment.replace('\\n', '\n')
                data.append(comment + '\n')
            data.extend(write_piece(split_to_merged(original_aa), 0, prof, prof))
        else:
            if profile_data[name].get('initial_comment', False):
                comment = profile_data[name]['initial_comment']
                comment.replace('\\n', '\n')
                data.append(comment + '\n')

            data.extend(write_piece(profile_data, 0, name, name))

    string += '\n'.join(data)

    return string + '\n'


def write_profile_ui_feedback(profile, is_attachment=False):
    aaui.UI_Info(_('Writing updated profile for %s.') % profile)
    write_profile(profile, is_attachment)


def write_profile(profile, is_attachment=False):
    if aa[profile][profile].get('filename', False):
        prof_filename = aa[profile][profile]['filename']
    elif is_attachment:
        prof_filename = get_profile_filename_from_attachment(profile, True)
    else:
        prof_filename = get_profile_filename_from_profile_name(profile, True)

    serialize_options = {'METADATA': True, 'is_attachment': is_attachment}
    profile_string = serialize_profile(split_to_merged(aa), profile, serialize_options)
    try:
        with NamedTemporaryFile('w', suffix='~', delete=False, dir=profile_dir) as newprof:
            if os.path.exists(prof_filename):
                shutil.copymode(prof_filename, newprof.name)
            else:
                # permission_600 = stat.S_IRUSR | stat.S_IWUSR    # Owner read and write
                # os.chmod(newprof.name, permission_600)
                pass
            newprof.write(profile_string)
    except PermissionError as e:
        raise AppArmorException(e)

    os.rename(newprof.name, prof_filename)

    if profile in changed:
        changed.pop(profile)
    else:
        debug_logger.info("Unchanged profile written: %s (not listed in 'changed' list)" % profile)

    original_aa[profile] = deepcopy(aa[profile])


def include_list_recursive(profile, in_preamble=False):
    """get a list of all includes in a profile and its included files"""

    includelist = profile['inc_ie'].get_all_full_paths(profile_dir)
    full_list = []

    while includelist:
        incname = includelist.pop(0)

        if incname in full_list:
            continue
        full_list.append(incname)

        if in_preamble:
            look_at = active_profiles.files[incname]
        else:
            look_at = include[incname][incname]

        for childinc in look_at['inc_ie'].rules:
            for childinc_file in childinc.get_full_paths(profile_dir):
                if childinc_file not in full_list:
                    includelist.append(childinc_file)

    return full_list


def is_known_rule(profile, rule_type, rule_obj):
    # XXX get rid of get() checks after we have a proper function to initialize a profile
    if profile.get(rule_type, False):
        if profile[rule_type].is_covered(rule_obj, False):
            return True

    includelist = include_list_recursive(profile)

    for incname in includelist:
        if include[incname][incname][rule_type].is_covered(rule_obj, False):
            return True

    return False


def get_file_perms(profile, path, audit, deny):
    """get the current permissions for the given path"""

    perms = profile['file'].get_perms_for_path(path, audit, deny)

    includelist = include_list_recursive(profile)

    for incname in includelist:
        incperms = include[incname][incname]['file'].get_perms_for_path(path, audit, deny)

        for allow_or_deny in ('allow', 'deny'):
            for owner_or_all in ('all', 'owner'):
                for perm in incperms[allow_or_deny][owner_or_all]:
                    perms[allow_or_deny][owner_or_all].add(perm)

                if 'a' in perms[allow_or_deny][owner_or_all] and 'w' in perms[allow_or_deny][owner_or_all]:
                    perms[allow_or_deny][owner_or_all].remove('a')  # a is a subset of w, so remove it

        for incpath in incperms['paths']:
            perms['paths'].add(incpath)

    return perms


def propose_file_rules(profile_obj, rule_obj):
    """Propose merged file rules based on the existing profile and the log events
       - permissions get merged
       - matching paths from existing rules, common_glob() and user_globs get proposed
       - IMPORTANT: modifies rule_obj.original_perms and rule_obj.perms"""
    options = []
    original_path = rule_obj.path.regex

    merged_rule_obj = deepcopy(rule_obj)   # make sure not to modify the original rule object (with exceptions, see end of this function)

    existing_perms = get_file_perms(profile_obj, rule_obj.path, False, False)
    for perm in existing_perms['allow']['all']:  # XXX also handle owner-only perms
        merged_rule_obj.perms.add(perm)
        merged_rule_obj.raw_rule = None

    if 'a' in merged_rule_obj.perms and 'w' in merged_rule_obj.perms:
        merged_rule_obj.perms.remove('a')  # a is a subset of w, so remove it

    pathlist = {original_path} | existing_perms['paths'] | set(glob_common(original_path))

    for user_glob in user_globs:
        if user_globs[user_glob].match(original_path):
            pathlist.add(user_glob)

    pathlist = order_globs(pathlist, original_path)

    # paths in existing rules that match the original path
    for path in pathlist:
        merged_rule_obj.store_edit(path)
        merged_rule_obj.raw_rule = None
        options.append(merged_rule_obj.get_clean())

    merged_rule_obj.exec_perms = None

    rule_obj.original_perms = existing_perms
    if rule_obj.perms != merged_rule_obj.perms:
        rule_obj.perms = merged_rule_obj.perms
        rule_obj.raw_rule = None

    return options


def reload_base(bin_path):
    if not check_for_apparmor():
        return

    prof_filename = get_profile_filename_from_profile_name(bin_path, True)

    reload_profile(prof_filename)


def reload_profile(prof_filename, raise_exc=False):
    """run apparmor_parser to reload the given profile file"""

    ret, out = cmd((parser, '-I%s' % profile_dir, '--base', profile_dir, '-r', prof_filename))

    if ret != 0:
        if raise_exc:
            raise AppArmorException(out)
        else:
            print(out)


def reload(bin_path):
    bin_path = find_executable(bin_path)
    if bin_path:
        reload_base(bin_path)


def get_include_data(filename):
    data = []
    if not filename.startswith('/'):
        filename = os.path.join(profile_dir, filename)
    if os.path.exists(filename):
        with open_file_read(filename) as f_in:
            data = f_in.readlines()
    else:
        raise AppArmorException(_('File Not Found: %s') % filename)
    return data


def include_dir_filelist(include_name):
    """returns a list of files in the given include_name directory,
       except skippable files.
    """

    if not include_name.startswith('/'):
        raise AppArmorBug('incfile %s not starting with /' % include_name)

    files = []
    for path in os.listdir(include_name):
        path = path.strip()
        if is_skippable_file(path):
            continue
        file_name = os.path.join(include_name, path)
        if os.path.isfile(file_name):
            files.append(file_name)

    return files


def load_include(incname, in_preamble=False):
    load_includeslist = [incname]
    while load_includeslist:
        incfile = load_includeslist.pop(0)
        if not incfile.startswith('/'):
            raise AppArmorBug('incfile %s not starting with /' % incfile)

        if include.get(incfile, {}).get(incfile, False):
            pass  # already read, do nothing
        elif os.path.isfile(incfile):
            data = get_include_data(incfile)
            incdata = parse_profile_data(data, incfile, True, in_preamble)
            attach_profile_data(include, incdata)
        # If the include is a directory means include all subfiles
        elif os.path.isdir(incfile):
            load_includeslist += include_dir_filelist(incfile)
        else:
            raise AppArmorException("Include file %s not found" % (incfile))

    return 0


def check_qualifiers(program):
    if cfg['qualifiers'].get(program, False):
        if cfg['qualifiers'][program] != 'p':
            fatal_error(
                _("%s is currently marked as a program that should not have its own\n"
                  "profile.  Usually, programs are marked this way if creating a profile for \n"
                  "them is likely to break the rest of the system.  If you know what you're\n"
                  "doing and are certain you want to create a profile for this program, edit\n"
                  "the corresponding entry in the [qualifiers] section in /etc/apparmor/logprof.conf.")
                % program)
    return False


def get_subdirectories(current_dir):
    """Returns a list of all directories directly inside given directory"""
    return next(os.walk(current_dir))[1]


def loadincludes():
    loadincludes_dir('tunables', True)
    loadincludes_dir('abstractions', False)


def loadincludes_dir(subdir, in_preamble):
    idir = os.path.join(profile_dir, subdir)

    if os.path.isdir(idir):  # if directory doesn't exist, silently skip loading it
        for dirpath, dirname, files in os.walk(idir):
            for fi in files:
                if is_skippable_file(fi):
                    continue
                else:
                    fi = os.path.join(dirpath, fi)
                    load_include(fi, in_preamble)


def glob_common(path):
    globs = []

    if re.search(r'[\d.]+\.so$', path) or re.search(r'\.so\.[\d.]+$', path):
        libpath = path
        libpath = re.sub(r'[\d.]+\.so$', '*.so', libpath)
        libpath = re.sub(r'\.so\.[\d.]+$', '.so.*', libpath)
        if libpath != path:
            globs.append(libpath)

    for glob in cfg['globs']:
        if re.search(glob, path):
            globbedpath = path
            globbedpath = re.sub(glob, cfg['globs'][glob], path)
            if globbedpath != path:
                globs.append(globbedpath)

    return sorted(set(globs))


def combine_name(name1, name2):
    if name1 == name2:
        return name1
    else:
        return '%s^%s' % (name1, name2)


def logger_path():
    logger = conf.find_first_file(cfg['settings']['logger']) or '/bin/logger'
    if not os.path.isfile(logger) or not os.access(logger, os.EX_OK):
        raise AppArmorException("Can't find logger!\nPlease make sure %s exists, or update the 'logger' path in logprof.conf." % logger)
    return logger


######Initialisations######

def init_aa(confdir=None, profiledir=None):
    global CONFDIR
    global conf
    global cfg
    global profile_dir
    global extra_profile_dir
    global parser

    if CONFDIR:
        return  # config already initialized (and possibly changed afterwards), so don't overwrite the config variables

    if not confdir:
        confdir = "/etc/apparmor"

    CONFDIR = confdir
    conf = apparmor.config.Config('ini', CONFDIR)
    cfg = conf.read_config('logprof.conf')

    # prevent various failures if logprof.conf doesn't exist
    if not cfg.sections():
        cfg.add_section('settings')
        cfg.add_section('required_hats')

    if cfg['settings'].get('default_owner_prompt', False):
        cfg['settings']['default_owner_prompt'] = ''

    if profiledir:
        profile_dir = profiledir
    else:
        profile_dir = conf.find_first_dir(cfg['settings'].get('profiledir')) or '/etc/apparmor.d'
    profile_dir = os.path.abspath(profile_dir)
    if not os.path.isdir(profile_dir):
        raise AppArmorException("Can't find AppArmor profiles in %s" % (profile_dir))

    extra_profile_dir = conf.find_first_dir(cfg['settings'].get('inactive_profiledir')) or '/usr/share/apparmor/extra-profiles/'

    parser = conf.find_first_file(cfg['settings'].get('parser')) or '/sbin/apparmor_parser'
    if not os.path.isfile(parser) or not os.access(parser, os.EX_OK):
        raise AppArmorException("Can't find apparmor_parser at %s" % (parser))
