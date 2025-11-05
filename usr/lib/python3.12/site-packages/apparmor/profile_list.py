# ----------------------------------------------------------------------
#    Copyright (C) 2018-2020 Christian Boltz <apparmor@cboltz.de>
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

from apparmor.aare import AARE
from apparmor.common import AppArmorBug, AppArmorException
from apparmor.profile_storage import ProfileStorage
from apparmor.rule.abi import AbiRule, AbiRuleset
from apparmor.rule.alias import AliasRule, AliasRuleset
from apparmor.rule.boolean import BooleanRule, BooleanRuleset
from apparmor.rule.include import IncludeRule, IncludeRuleset
from apparmor.rule.variable import VariableRule, VariableRuleset
from apparmor.translations import init_translation

_ = init_translation()

preamble_ruletypes = {
    'abi':      {'rule': AbiRule,      'ruleset': AbiRuleset},
    'alias':    {'rule': AliasRule,    'ruleset': AliasRuleset},
    'inc_ie':   {'rule': IncludeRule,  'ruleset': IncludeRuleset},
    'variable': {'rule': VariableRule, 'ruleset': VariableRuleset},
    'boolean':  {'rule': BooleanRule,  'ruleset': BooleanRuleset},
}
header_rule_write_order = ('abi', 'alias', 'inc_ie', 'variable', 'boolean')  # TODO: Dicts are ordered in Python 3.7+; use above dict's keys instead


class ProfileList:
    """Stores the preamble section and the list of profile(s) (both name and
       attachment) that live in profile files.

       Also allows "reverse" lookups to find out in which file a profile
       lives.
    """

    def __init__(self):
        self.profile_names = {}     # profile name -> filename
        self.attachments = {}       # attachment -> {'f': filename, 'p': profile}
        self.attachments_AARE = {}  # attachment -> AARE(attachment)
        self.files = {}             # filename -> content - see init_file()
        self.profiles = {}          # profile_name -> ProfileStorage

    def __repr__(self):
        return ('\n<ProfileList>\n%s\n</ProfileList>\n' % '\n'.join(self.files))

    def init_file(self, filename):
        if self.files.get(filename):
            return  # don't re-initialize / overwrite existing data

        self.files[filename] = {
            'profiles': [],
        }

        for rule in preamble_ruletypes:
            self.files[filename][rule] = preamble_ruletypes[rule]['ruleset']()

    def add_profile(self, filename, profile_name, attachment, prof_storage=None):
        """Add the given profile and attachment to the list"""

        if not filename:
            raise AppArmorBug('Empty filename given to ProfileList')

        if not profile_name and not attachment:
            raise AppArmorBug('Neither profile name or attachment given')

        if type(prof_storage) is not ProfileStorage and prof_storage is not None:
            raise AppArmorBug('Invalid profile type: %s' % type(prof_storage))

        if profile_name in self.profile_names:
            raise AppArmorException(
                _('Profile %(profile_name)s exists in %(filename)s and %(filename2)s'
                  % {'profile_name': profile_name, 'filename': filename, 'filename2': self.profile_names[profile_name]}))

        if attachment in self.attachments:
            raise AppArmorException(
                _('Profile for %(profile_name)s exists in %(filename)s and %(filename2)s'
                  % {'profile_name': attachment, 'filename': filename, 'filename2': self.attachments[attachment]}))

        if profile_name:
            self.profile_names[profile_name] = filename

        if attachment:
            self.attachments[attachment] = {'f': filename, 'p': profile_name or attachment}  # if a profile doesn't have a name, the attachment is stored as profile name
            self.attachments_AARE[attachment] = AARE(attachment, True)

        self.init_file(filename)

        if profile_name:
            self.files[filename]['profiles'].append(profile_name)
            self.profiles[profile_name] = prof_storage
        else:
            self.files[filename]['profiles'].append(attachment)
            self.profiles[attachment] = prof_storage

    def add_rule(self, filename, ruletype, rule):
        """Store the given rule for the given profile filename preamble"""

        self.init_file(filename)

        self.files[filename][ruletype].add(rule)

    def add_abi(self, filename, abi_rule):
        """Store the given abi rule for the given profile filename preamble"""

        if type(abi_rule) is not AbiRule:
            raise AppArmorBug('Wrong type given to ProfileList: %s' % abi_rule)

        self.init_file(filename)

        self.files[filename]['abi'].add(abi_rule)

    def add_alias(self, filename, alias_rule):
        """Store the given alias rule for the given profile filename preamble"""

        if type(alias_rule) is not AliasRule:
            raise AppArmorBug('Wrong type given to ProfileList: %s' % alias_rule)

        self.init_file(filename)

        self.files[filename]['alias'].add(alias_rule)

    def add_inc_ie(self, filename, inc_rule):
        """Store the given include / include if exists rule for the given profile filename preamble"""
        if type(inc_rule) is not IncludeRule:
            raise AppArmorBug('Wrong type given to ProfileList: %s' % inc_rule)

        self.init_file(filename)

        self.files[filename]['inc_ie'].add(inc_rule)

    def add_variable(self, filename, var_rule):
        """Store the given variable rule for the given profile filename preamble"""
        if type(var_rule) is not VariableRule:
            raise AppArmorBug('Wrong type given to ProfileList: %s' % var_rule)

        self.init_file(filename)

        self.files[filename]['variable'].add(var_rule)

    def add_boolean(self, filename, bool_rule):
        """Store the given boolean variable rule for the given profile filename preamble"""
        if type(bool_rule) is not BooleanRule:
            raise AppArmorBug('Wrong type given to ProfileList: %s' % bool_rule)

        self.init_file(filename)

        self.files[filename]['boolean'].add(bool_rule)

    def delete_preamble_duplicates(self, filename):
        """Delete duplicates in the preamble of the given profile file"""

        if not self.files.get(filename):
            raise AppArmorBug('%s not listed in ProfileList files' % filename)

        deleted = 0

        for r_type in preamble_ruletypes:
            deleted += self.files[filename][r_type].delete_duplicates(None)  # None means not to check includes -- TODO check if this makes sense for all preamble rule types

        return deleted

    def get_profile_and_childs(self, profile_name):
        found = {}
        for prof in self.profiles:
            if prof == profile_name or prof.startswith('%s//' % profile_name):
                found[prof] = self.profiles[prof]

        return found

    def get_raw(self, filename, depth=0):
        """Get the preamble for the given profile filename (in original formatting)"""
        if not self.files.get(filename):
            raise AppArmorBug('%s not listed in ProfileList files' % filename)

        data = []
        for rule_type in header_rule_write_order:
            data.extend(self.files[filename][rule_type].get_raw(depth))
        return data

    def get_clean(self, filename, depth=0):
        """Get the preamble for the given profile filename (in clean formatting)"""
        if not self.files.get(filename):
            raise AppArmorBug('%s not listed in ProfileList files' % filename)

        data = []
        for rule_type in header_rule_write_order:
            data.extend(self.files[filename][rule_type].get_clean_unsorted(depth))
        return data

    def filename_from_profile_name(self, name):
        """Return profile filename for the given profile name, or None"""

        return self.profile_names.get(name, None)

    def filename_from_attachment(self, attachment):
        """Return profile filename for the given attachment/executable path, or None"""
        return self.thing_from_attachment(attachment, 'f')

    def profile_from_attachment(self, attachment):
        """Return profile filename for the given attachment/executable path, or None"""
        return self.thing_from_attachment(attachment, 'p')

    def thing_from_attachment(self, attachment, thing):
        """Return thing for the given attachment/executable path, or None.

           thing can be 'f' for filename or 'p' for profile name"""

        if not attachment.startswith(('/', '@', '{')):
            raise AppArmorBug('Called filename_from_attachment with non-path attachment: %s' % attachment)

        # plain path
        if self.attachments.get(attachment):
            return self.attachments[attachment][thing]

        # try AARE matches to cover profile names with alternations and wildcards
        for path in self.attachments.keys():
            if self.attachments_AARE[path].match(attachment):
                return self.attachments[path][thing]  # XXX this returns the first match, not necessarily the best one

        return None  # nothing found

    def get_all_merged_variables(self, filename, all_incfiles):
        """Get merged variables of a file and its includes

           Note that this function is more forgiving than apparmor_parser.
           It detects variable redefinitions and adding values to non-existing variables.
           However, it doesn't honor the order - so adding to a variable first and defining
           it later won't trigger an error.
        """

        if not self.files.get(filename):
            raise AppArmorBug('%s not listed in ProfileList files' % filename)

        merged_variables = {}

        mainfile_variables = self.files[filename]['variable'].get_merged_variables()

        # keep track in which file a variable gets set
        set_in = {}
        for var in mainfile_variables['=']:
            merged_variables[var] = mainfile_variables['='][var]
            set_in[var] = filename

        # collect variable additions (+=)
        inc_add = {}
        if mainfile_variables['+=']:
            inc_add[filename] = mainfile_variables['+=']  # variable additions from main file

        for incname in all_incfiles:
            inc_vars = self.files[incname]['variable'].get_merged_variables()

            for var in inc_vars['=']:
                if merged_variables.get(var):
                    raise AppArmorException(
                        'While parsing %(profile)s: Conflicting variable definitions for variable %(var)s found in %(file1)s and %(file2)s.'
                        % {'var': var, 'profile': filename, 'file1': set_in[var], 'file2': incname})
                else:
                    merged_variables[var] = inc_vars['='][var]
                    set_in[var] = incname

            # variable additions can happen in other files than the variable definition. First collect them from all files...
            if inc_vars['+=']:
                inc_add[incname] = inc_vars['+=']

        for incname in inc_add:
            # ... and then check if the variables that get extended have an initial definition. If yes, merge them.
            for var in inc_add[incname]:
                if merged_variables.get(var):
                    merged_variables[var] |= inc_add[incname][var]
                else:
                    raise AppArmorException(
                        'While parsing %(profile)s: Variable %(var)s was not previously declared, but is being assigned additional value in file %(file)s.'
                        % {'var': var, 'profile': filename, 'file': incname})

        return merged_variables

    def profiles_in_file(self, filename):
        """Return list of profiles in the given file"""
        if not self.files.get(filename):
            raise AppArmorBug('%s not listed in ProfileList files' % filename)

        return self.files[filename]['profiles']
