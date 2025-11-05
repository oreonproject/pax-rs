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

from apparmor.common import AppArmorBug, AppArmorException
from apparmor.regex import RE_PROFILE_ALIAS, strip_quotes
from apparmor.rule import BaseRule, BaseRuleset, parse_comment, quote_if_needed
from apparmor.translations import init_translation

_ = init_translation()


class AliasRule(BaseRule):
    """Class to handle and store a single alias rule"""

    rule_name = 'alias'

    def __init__(self, orig_path, target, audit=False, deny=False, allow_keyword=False,
                 comment='', log_event=None):

        super().__init__(audit=audit, deny=deny, allow_keyword=allow_keyword,
                         comment=comment, log_event=log_event)

        # aliass don't support audit or deny
        if audit:
            raise AppArmorBug('Attempt to initialize %s with audit flag' % self.__class__.__name__)
        if deny:
            raise AppArmorBug('Attempt to initialize %s with deny flag' % self.__class__.__name__)

        if type(orig_path) is not str:
            raise AppArmorBug('Passed unknown type for orig_path to %s: %s' % (self.__class__.__name__, orig_path))
        if not orig_path:
            raise AppArmorException('Passed empty orig_path to %s: %s' % (self.__class__.__name__, orig_path))
        if not orig_path.startswith('/'):
            raise AppArmorException("Alias path doesn't start with '/'")

        if type(target) is not str:
            raise AppArmorBug('Passed unknown type for target to %s: %s' % (self.__class__.__name__, target))
        if not target:
            raise AppArmorException('Passed empty target to %s: %s' % (self.__class__.__name__, target))
        if not target.startswith('/'):
            raise AppArmorException("Alias target doesn't start with '/'")

        self.orig_path = orig_path
        self.target = target

    @classmethod
    def _match(cls, raw_rule):
        return RE_PROFILE_ALIAS.search(raw_rule)

    @classmethod
    def _parse(cls, raw_rule):
        """parse raw_rule and return AliasRule"""

        matches = cls._match(raw_rule)
        if not matches:
            raise AppArmorException(_("Invalid alias rule '%s'") % raw_rule)

        comment = parse_comment(matches)

        orig_path = strip_quotes(matches.group('orig_path').strip())
        target = strip_quotes(matches.group('target').strip())

        return AliasRule(orig_path, target,
                         audit=False, deny=False, allow_keyword=False, comment=comment)

    def get_clean(self, depth=0):
        """return rule (in clean/default formatting)"""

        space = '  ' * depth

        return '%salias %s -> %s,' % (space, quote_if_needed(self.orig_path), quote_if_needed(self.target))

    def is_covered_localvars(self, other_rule):
        """check if other_rule is covered by this rule object"""

        # the only way aliases can be covered are exact duplicates
        return self.is_equal_localvars(other_rule, False)

    def is_equal_localvars(self, rule_obj, strict):
        """compare if rule-specific aliass are equal"""

        if not type(rule_obj) == AliasRule:
            raise AppArmorBug('Passed non-alias rule: %s' % str(rule_obj))

        if self.orig_path != rule_obj.orig_path:
            return False

        if self.target != rule_obj.target:
            return False

        return True

    def logprof_header_localvars(self):
        headers = []

        return headers + [
            _('Alias'), '%s -> %s' % (self.orig_path, self.target),
        ]


class AliasRuleset(BaseRuleset):
    """Class to handle and store a collection of alias rules"""
