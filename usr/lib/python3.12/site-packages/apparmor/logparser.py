# ----------------------------------------------------------------------
#    Copyright (C) 2013 Kshitij Gupta <kgupta8592@gmail.com>
#    Copyright (C) 2015-2019 Christian Boltz <apparmor@cboltz.de>
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
import ctypes
import re
import sys
import time

import LibAppArmor
from apparmor.common import AppArmorBug, AppArmorException, DebugLogger, hasher, open_file_read, split_name
from apparmor.translations import init_translation

_ = init_translation()


class ReadLog:

    # used to pre-filter log lines so that we hand over only relevant lines to LibAppArmor parsing
    RE_LOG_ALL = re.compile('apparmor=|operation=|type=AVC')

    def __init__(self, filename, active_profiles, profile_dir):
        self.filename = filename
        self.profile_dir = profile_dir
        self.active_profiles = active_profiles
        self.hashlog = {'PERMITTING': {}, 'REJECTING': {}}  # structure inside {}: {'profilename': init_hashlog(aamode, profilename), 'profilename2': init_hashlog(...), ...}
        self.debug_logger = DebugLogger('ReadLog')
        self.LOG = None
        self.logmark = ''
        self.seenmark = None
        self.next_log_entry = None

    def init_hashlog(self, aamode, profile):
        """initialize self.hashlog[aamode][profile] for all rule types"""

        if profile in self.hashlog[aamode].keys():
            return  # already initialized, don't overwrite existing data

        self.hashlog[aamode][profile] = {
            'final_name':   profile,  # might be changed for null-* profiles based on exec decisions
            'capability':   {},  # flat, no hasher needed
            'change_hat':   {},  # flat, no hasher needed
            'change_profile': {},  # flat, no hasher needed  (at least in logparser which doesn't support EXEC MODE and EXEC COND)
            'dbus':         hasher(),
            'exec':         hasher(),
            'network':      hasher(),
            'path':         hasher(),
            'ptrace':       hasher(),
            'signal':       hasher(),
        }

    def prefetch_next_log_entry(self):
        if self.next_log_entry:
            sys.stderr.out('A log entry already present: %s' % self.next_log_entry)
        self.next_log_entry = self.LOG.readline()
        while not self.RE_LOG_ALL.search(self.next_log_entry) and not (self.logmark and self.logmark in self.next_log_entry):
            self.next_log_entry = self.LOG.readline()
            if not self.next_log_entry:
                break

    def get_next_log_entry(self):
        # If no next log entry fetch it
        if not self.next_log_entry:
            self.prefetch_next_log_entry()
        log_entry = self.next_log_entry
        self.next_log_entry = None
        return log_entry

    def parse_event(self, msg):
        """Parse the event from log into key value pairs"""
        msg = msg.strip()
        self.debug_logger.info('parse_event: %s' % msg)
        event = LibAppArmor.parse_record(msg)
        ev = dict()
        ev['resource'] = event.info
        ev['active_hat'] = event.active_hat
        ev['aamode'] = event.event
        ev['time'] = event.epoch
        ev['operation'] = event.operation
        ev['profile'] = event.profile
        ev['name'] = event.name
        ev['name2'] = event.name2
        ev['attr'] = event.attribute
        ev['parent'] = event.parent
        ev['pid'] = event.pid
        ev['task'] = event.task
        ev['info'] = event.info
        ev['error_code'] = event.error_code
        ev['denied_mask'] = event.denied_mask
        ev['request_mask'] = event.requested_mask
        ev['magic_token'] = event.magic_token
        ev['family'] = event.net_family
        ev['protocol'] = event.net_protocol
        ev['sock_type'] = event.net_sock_type
        ev['class'] = event._class

        if event.ouid != ctypes.c_ulong(-1).value:  # ULONG_MAX
            ev['fsuid'] = event.fsuid
            ev['ouid'] = event.ouid

        if ev['operation'] and ev['operation'] == 'signal':
            ev['signal'] = event.signal
            ev['peer'] = event.peer
        elif ev['operation'] and ev['operation'] == 'ptrace':
            ev['peer'] = event.peer
        elif ev['operation'] and ev['operation'].startswith('dbus_'):
            ev['peer_profile'] = event.peer_profile
            ev['bus'] = event.dbus_bus
            ev['path'] = event.dbus_path
            ev['interface'] = event.dbus_interface
            ev['member'] = event.dbus_member

        LibAppArmor.free_record(event)

        if not ev['time']:
            ev['time'] = int(time.time())

        if ev['aamode']:
            # Convert aamode values to their counter-parts
            mode_convertor = {0: 'UNKNOWN',
                              1: 'ERROR',
                              2: 'AUDIT',
                              3: 'PERMITTING',
                              4: 'REJECTING',
                              5: 'HINT',
                              6: 'STATUS'
                              }
            try:
                ev['aamode'] = mode_convertor[ev['aamode']]
            except KeyError:
                ev['aamode'] = None

        # "translate" disconnected paths to errors, which means the event will be ignored.
        # XXX Ideally we should propose to add the attach_disconnected flag to the profile
        if ev['error_code'] == 13 and ev['info'] == 'Failed name lookup - disconnected path':
            ev['aamode'] = 'ERROR'

        if ev['aamode']:
            return ev
        else:
            return None

    def parse_event_for_tree(self, e):
        aamode = e.get('aamode', 'UNKNOWN')

        if aamode == 'UNKNOWN':
            raise AppArmorBug('aamode is UNKNOWN - %s' % e['type'])  # should never happen

        if aamode in ('AUDIT', 'STATUS', 'ERROR'):
            return

        # Skip if AUDIT event was issued due to a change_hat in unconfined mode
        if not e.get('profile', False):
            return

        full_profile = e['profile']  # full, nested profile name
        self.init_hashlog(aamode, full_profile)

        # Convert new null profiles to old single level null profile
        if '//null-' in e['profile']:
            e['profile'] = 'null-complain-profile'

        profile, hat = split_name(e['profile'])

        if profile != 'null-complain-profile' and not self.profile_exists(profile):
            return
        if e['operation'] == 'exec':
            if not e['name']:
                raise AppArmorException('exec without executed binary')

            if not e['name2']:
                e['name2'] = ''  # exec events in enforce mode don't have target=...

            self.hashlog[aamode][full_profile]['exec'][e['name']][e['name2']] = True
            return

        elif self.op_type(e) == 'file':
            # Map c (create) and d (delete) to w (logging is more detailed than the profile language)
            dmask = e['denied_mask']
            dmask = dmask.replace('c', 'w')
            dmask = dmask.replace('d', 'w')

            owner = False

            if '::' in dmask:
                # old log styles used :: to indicate if permissions are meant for owner or other
                (owner_d, other_d) = dmask.split('::')
                if owner_d and other_d:
                    raise AppArmorException('Found log event with both owner and other permissions. Please open a bugreport!')
                if owner_d:
                    dmask = owner_d
                    owner = True
                else:
                    dmask = other_d

            if e.get('ouid') is not None and e['fsuid'] == e['ouid']:
                # in current log style, owner permissions are indicated by a match of fsuid and ouid
                owner = True

            if 'x' in dmask and dmask != 'x':
                dmask = dmask.replace('x', '')  # if dmask contains x and another mode, drop x here - we should see a separate exec event

            for perm in dmask:
                if perm in 'mrwalk':  # intentionally not allowing 'x' here
                    self.hashlog[aamode][full_profile]['path'][e['name']][owner][perm] = True
                else:
                    raise AppArmorException(_('Log contains unknown mode %s') % dmask)

            return

        elif e['operation'] == 'capable':
            self.hashlog[aamode][full_profile]['capability'][e['name']] = True
            return

        elif self.op_type(e) == 'net':
            self.hashlog[aamode][full_profile]['network'][e['family']][e['sock_type']][e['protocol']] = True
            return

        elif e['operation'] == 'change_hat':
            if e['error_code'] == 1 and e['info'] == 'unconfined can not change_hat':
                return

            self.hashlog[aamode][full_profile]['change_hat'][e['name2']] = True
            return

        elif e['operation'] == 'change_profile':
            self.hashlog[aamode][full_profile]['change_profile'][e['name2']] = True
            return

        elif e['operation'] == 'ptrace':
            if not e['peer']:
                self.debug_logger.debug('ignored garbage ptrace event with empty peer')
                return
            if not e['denied_mask']:
                self.debug_logger.debug('ignored garbage ptrace event with empty denied_mask')
                return

            self.hashlog[aamode][full_profile]['ptrace'][e['peer']][e['denied_mask']] = True
            return

        elif e['operation'] == 'signal':
            self.hashlog[aamode][full_profile]['signal'][e['peer']][e['denied_mask']][e['signal']] = True
            return

        elif e['operation'] and e['operation'].startswith('dbus_'):
            self.hashlog[aamode][full_profile]['dbus'][e['denied_mask']][e['bus']][e['path']][e['name']][e['interface']][e['member']][e['peer_profile']] = True
            return

        else:
            self.debug_logger.debug('UNHANDLED: %s' % e)

    def read_log(self, logmark):
        self.logmark = logmark
        seenmark = True
        if self.logmark:
            seenmark = False
        try:
            self.LOG = open_file_read(self.filename)
        except IOError:
            raise AppArmorException('Can not read AppArmor logfile: ' + self.filename)
        with self.LOG:
            line = True
            while line:
                line = self.get_next_log_entry()
                if not line:
                    break
                line = line.strip()
                self.debug_logger.debug('read_log: %s' % line)
                if self.logmark in line:
                    seenmark = True

                self.debug_logger.debug('read_log: seenmark = %s' % seenmark)
                if not seenmark:
                    continue

                event = self.parse_event(line)
                if event:
                    try:
                        self.parse_event_for_tree(event)

                    except AppArmorException as e:
                        ex_msg = ('%(msg)s\n\nThis error was caused by the log line:\n%(logline)s'
                                  % {'msg': e.value, 'logline': line})
                        raise AppArmorBug(ex_msg) from None

        self.logmark = ''

        return self.hashlog

    # operation types that can be network or file operations
    # (used by op_type() which checks some event details to decide)
    OP_TYPE_FILE_OR_NET = {
        # Note: op_type() also uses some startswith() checks which are not listed here!
        'create',
        'post_create',
        'bind',
        'connect',
        'listen',
        'accept',
        'sendmsg',
        'recvmsg',
        'getsockname',
        'getpeername',
        'getsockopt',
        'setsockopt',
        'socket_create',
        'sock_shutdown',
        'open',
        'truncate',
        'mkdir',
        'mknod',
        'chmod',
        'chown',
        'rename_src',
        'rename_dest',
        'unlink',
        'rmdir',
        'symlink',
        'symlink_create',
        'link',
        'sysctl',
        'getattr',
        'setattr',
        'xattr',
    }

    def op_type(self, event):
        """Returns the operation type if known, unknown otherwise"""

        if event['operation'] and (event['operation'].startswith('file_') or
                                   event['operation'].startswith('inode_') or
                                   event['operation'] in self.OP_TYPE_FILE_OR_NET):
            # file or network event?
            if event['family'] and event['protocol'] and event['sock_type']:
                # 'unix' events also use keywords like 'connect', but protocol is 0 and should therefore be filtered out
                return 'net'
            elif event['denied_mask']:
                return 'file'
            else:
                raise AppArmorException('unknown file or network event type')

        else:
            return 'unknown'

    def profile_exists(self, program):
        """Returns True if profile exists, False otherwise"""
        # Check cache of profiles
        if self.active_profiles.filename_from_profile_name(program):
            return True

        return False
