Ext.define('PBS.SystemConfiguration', {
    extend: 'Ext.tab.Panel',
    xtype: 'pbsSystemConfiguration',

    title: gettext('Configuration') + ': ' + gettext('System'),
    border: true,
    scrollable: true,
    defaults: { border: false },
    tools: [PBS.Utils.get_help_tool("sysadmin-network-configuration")],
    items: [
	{
	    xtype: 'panel',
	    title: gettext(' Network/Time'),
	    itemId: 'network',
	    layout: {
		type: 'vbox',
		align: 'stretch',
		multi: true,
	    },
	    defaults: {
		collapsible: true,
		animCollapse: false,
		margin: '10 10 0 10',
	    },
	    items: [
		{
		    xtype: 'proxmoxNodeTimeView',
		    title: gettext('Time'),
		    nodename: 'localhost',
		},
		{
		    xtype: 'proxmoxNodeDNSView',
		    title: gettext('DNS'),
		    nodename: 'localhost',
		},
		{
		    xtype: 'proxmoxNodeNetworkView',
		    title: gettext('Network Interfaces'),
		    flex: 1,
		    minHeight: 200,
		    showApplyBtn: true,
		    types: ['bond', 'bridge'],
		    nodename: 'localhost',
		},
	    ],
	},
	{
	    xtype: 'panel',
	    title: gettext('Other'),
	    itemId: 'other-options',
	    layout: {
		type: 'vbox',
		align: 'stretch',
		multi: true,
	    },
	    defaults: {
		collapsible: true,
		animCollapse: false,
		margin: '10 10 0 10',
	    },
	    items: [
		{
		    title: gettext('General'),
		    xtype: 'pbsNodeOptionView',
		},
		{
		    title: gettext('WebAuthn TFA'),
		    xtype: 'pbsWebauthnConfigView',
		},
	    ],
	},
    ],

    initComponent: function() {
	let me = this;

	me.callParent();

	let networktime = me.getComponent('network');
	networktime.query()?.forEach(el => el.relayEvents(networktime, ['activate', 'deactivate', 'destroy']));

	let options = me.getComponent('other-options');
	options.query()?.forEach(el => el.relayEvents(options, ['activate', 'deactivate', 'destroy']));
    },
});


