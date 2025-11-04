# ----------------------------------------------------------------------
#    Copyright (C) 2020 Christian Boltz <apparmor@cboltz.de>
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

from apparmor.common import AppArmorBug, AppArmorException, is_skippable_file
from apparmor.regex import RE_INCLUDE, re_match_include_parse
from apparmor.rule import BaseRule, BaseRuleset, parse_comment
from apparmor.translations import init_translation

_ = init_translation()


class IncludeRule(BaseRule):
    """Class to handle and store a single include rule"""

    rule_name = 'include'

    def __init__(self, path, ifexists, ismagic, audit=False, deny=False, allow_keyword=False,
                 comment='', log_event=None):

        super().__init__(audit=audit, deny=deny, allow_keyword=allow_keyword,
                         comment=comment, log_event=log_event)

        # include doesn't support audit or deny
        if audit:
            raise AppArmorBug('Attempt to initialize %s with audit flag' % self.__class__.__name__)
        if deny:
            raise AppArmorBug('Attempt to initialize %s with deny flag' % self.__class__.__name__)

        if type(ifexists) is not bool:
            raise AppArmorBug('Passed unknown type for ifexists to %s: %s' % (self.__class__.__name__, ifexists))
        if type(ismagic) is not bool:
            raise AppArmorBug('Passed unknown type for ismagic to %s: %s' % (self.__class__.__name__, ismagic))
        if type(path) is not str:
            raise AppArmorBug('Passed unknown type for path to %s: %s' % (self.__class__.__name__, path))
        if not path:
            raise AppArmorBug('Passed empty path to %s: %s' % (self.__class__.__name__, path))

        self.path = path
        self.ifexists = ifexists
        self.ismagic = ismagic

    @classmethod
    def _match(cls, raw_rule):
        return RE_INCLUDE.search(raw_rule)

    @classmethod
    def _parse(cls, raw_rule):
        """parse raw_rule and return IncludeRule"""

        matches = cls._match(raw_rule)
        if not matches:
            raise AppArmorException(_("Invalid %s rule '%s'") % (cls.rule_name, raw_rule))

        comment = parse_comment(matches)

        # TODO: move re_match_include_parse() from regex.py to this class after converting all code to use IncludeRule
        path, ifexists, ismagic = re_match_include_parse(raw_rule, cls.rule_name)

        return cls(path, ifexists, ismagic,
                   audit=False, deny=False, allow_keyword=False, comment=comment)

    def get_clean(self, depth=0):
        """return rule (in clean/default formatting)"""

        space = '  ' * depth

        ifexists_txt = ''
        if self.ifexists:
            ifexists_txt = ' if exists'

        if self.ismagic:
            return ('%s%s%s <%s>%s' % (space, self.rule_name, ifexists_txt, self.path, self.comment))
        else:
            return ('%s%s%s "%s"%s' % (space, self.rule_name, ifexists_txt, self.path, self.comment))

    def is_covered_localvars(self, other_rule):
        """check if other_rule is covered by this rule object"""

        if (self.path != other_rule.path):
            return False

        if (self.ifexists != other_rule.ifexists) and self.ifexists:  # "if exists" is allowed to differ
            return False

        if (self.ismagic != other_rule.ismagic):
            return False

        # still here? -> then it is covered
        return True

    def is_equal_localvars(self, rule_obj, strict):
        """compare if rule-specific variables are equal"""

        if not type(rule_obj) == type(self):
            raise AppArmorBug('Passed non-%s rule: %s' % (self.rule_name, str(rule_obj)))

        if (self.path != rule_obj.path):
            return False

        if (self.ifexists != rule_obj.ifexists):
            return False

        if (self.ismagic != rule_obj.ismagic):
            return False

        return True

    def logprof_header_localvars(self):
        return [_('Include'), self.get_clean()]

    def get_full_paths(self, profile_dir):
        """get list of full paths of an include (can contain multiple paths if self.path is a directory)"""

        # TODO: improve/fix logic to honor magic vs. quoted include paths
        if self.path.startswith('/'):
            full_path = self.path
        else:
            full_path = os.path.join(profile_dir, self.path)

        files = []

        if os.path.isdir(full_path):
            for path in sorted(os.listdir(full_path)):
                if is_skippable_file(path):
                    continue

                file_name = os.path.join(full_path, path)
                if os.path.isfile(file_name):  # only add files, but not subdirectories etc.
                    files.append(file_name)

        elif os.path.exists(full_path):
            files.append(full_path)

        elif not self.ifexists:
            files.append(full_path)  # add full_path even if it doesn't exist on disk. Might cause a 'file not found' error later.

        return files


class IncludeRuleset(BaseRuleset):
    """Class to handle and store a collection of include rules"""

    def get_all_full_paths(self, profile_dir):
        """get full path of all includes"""

        paths = []
        for rule_obj in self.rules:
            paths.extend(rule_obj.get_full_paths(profile_dir))

        return paths
