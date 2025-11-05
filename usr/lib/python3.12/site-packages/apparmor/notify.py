# ----------------------------------------------------------------------
#    Copyright (C) 2018–2019 Otto Kekäläinen <otto@kekalainen.net>
#    Copyright (C) 2021 Christian Boltz
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

import os
import struct

from apparmor.common import AppArmorBug, DebugLogger

debug_logger = DebugLogger('apparmor.notify')


def sane_timestamp(timestamp):
    """Check if the given timestamp is in a date range that makes sense for a wtmp file"""

    if timestamp < 946681200:  # 2000-01-01
        return False
    elif timestamp > 2524604400:  # 2050-01-01
        return False

    return True


def get_last_login_timestamp(username, filename='/var/log/wtmp'):
    """Directly read wtmp and get last login for user as epoch timestamp"""
    timestamp = 0
    last_login = 0

    debug_logger.debug('Username: {}'.format(username))

    with open(filename, "rb") as wtmp_file:
        offset = 0
        wtmp_filesize = os.path.getsize(filename)
        debug_logger.debug('WTMP filesize: {}'.format(wtmp_filesize))

        if wtmp_filesize < 356:
            return 0  # (nearly) empty wtmp file, no entries

        # detect architecture based on utmp format differences
        wtmp_file.seek(340)  # first possible timestamp position
        timestamp_x86_64  = struct.unpack("<L", wtmp_file.read(4))[0]  # noqa: E221
        timestamp_aarch64 = struct.unpack("<L", wtmp_file.read(4))[0]
        timestamp_s390x   = struct.unpack(">L", wtmp_file.read(4))[0]  # noqa: E221
        debug_logger.debug('WTMP timestamps: x86_64 %s, aarch64 %s, s390x %s'
                           % (timestamp_x86_64, timestamp_aarch64, timestamp_s390x))

        if sane_timestamp(timestamp_x86_64):
            endianness = '<'  # little endian
            extra_offset_before = 0
            extra_offset_after = 0
        elif sane_timestamp(timestamp_aarch64):
            endianness = '<'  # little endian
            extra_offset_before = 4
            extra_offset_after = 12
        elif sane_timestamp(timestamp_s390x):
            endianness = '>'  # big endian
            extra_offset_before = 8
            extra_offset_after = 8
        else:
            raise AppArmorBug(
                'Your /var/log/wtmp is broken or has an unknown format. '
                'Please open a bugreport with /var/log/wtmp and the output of "last" attached!')

        while offset < wtmp_filesize:
            wtmp_file.seek(offset)
            offset += 384 + extra_offset_before + extra_offset_after  # Increment for next entry

            type = struct.unpack('%sH' % endianness, wtmp_file.read(2))[0]
            debug_logger.debug('WTMP entry type: {}'.format(type))
            wtmp_file.read(2)  # skip padding

            # Only parse USER lines
            if type == 7:
                # Read each item and move pointer forward
                pid = struct.unpack("<L", wtmp_file.read(4))[0]
                line = wtmp_file.read(32).decode("utf-8", "replace").split('\0', 1)[0]
                id = wtmp_file.read(4).decode("utf-8", "replace").split('\0', 1)[0]
                user = wtmp_file.read(32).decode("utf-8", "replace").split('\0', 1)[0]
                host = wtmp_file.read(256).decode("utf-8", "replace").split('\0', 1)[0]
                term = struct.unpack("<H", wtmp_file.read(2))[0]
                exit = struct.unpack("<H", wtmp_file.read(2))[0]
                session = struct.unpack("<L", wtmp_file.read(4))[0]
                if extra_offset_before:
                    wtmp_file.read(extra_offset_before)
                timestamp = struct.unpack('%sL' % endianness, wtmp_file.read(4))[0]
                if extra_offset_after:
                    wtmp_file.read(extra_offset_after)
                usec = struct.unpack("<L", wtmp_file.read(4))[0]
                entry = (pid, line, id, user, host, term, exit, session, timestamp, usec)
                debug_logger.debug('WTMP entry: {}'.format(entry))

                # Store login timestamp for requested user
                if user == username:
                    last_login = timestamp

    # When loop is done, last value should be the latest login timestamp
    return last_login
