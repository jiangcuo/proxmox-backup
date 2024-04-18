Ext.define('pbs-gc-jobs-status', {
    extend: 'Ext.data.Model',
    fields: [
	'store', 'last-run-upid', 'removed-chunks', 'pending-chunks', 'schedule',
	'next-run', 'last-run-endtime', 'last-run-state',
	{
	    name: 'duration',
	    calculate: function(data) {
		let endtime = data['last-run-endtime'];
		if (!endtime) return undefined;
		let task = Proxmox.Utils.parse_task_upid(data['last-run-upid']);
		return endtime - task.starttime;
	    },
	},
    ],
    idProperty: 'store',
    proxy: {
	type: 'proxmox',
	url: '/api2/json/admin/gc',
    },
});

Ext.define('PBS.config.GCJobView', {
    extend: 'Ext.grid.GridPanel',
    alias: 'widget.pbsGCJobView',

    stateful: true,
    stateId: 'grid-gc-jobs-v1',
    allowDeselect: false,

    title: gettext('Garbage Collect Jobs'),

    controller: {
	xclass: 'Ext.app.ViewController',

	init: function(view) {
	    let params = {};
	    let store = view.getStore();
	    let proxy = store.rstore.getProxy();
	    if (view.datastore) {
		params.store = view.datastore;

		// after the store is loaded, select the row to enable the Edit,.. buttons
		store.rstore.proxy.on({
		    'afterload': {
			fn: () => view.getSelectionModel().select(0),
			single: true,
		    },
		});

		// do not highlight the selected row
		view.items.items[0].selectedItemCls = '';
		view.items.items[0].overItemCls = '';
	    }
	    proxy.setExtraParams(params);
	    Proxmox.Utils.monStoreErrors(view, store.rstore);
	},

	getDatastoreName: function() {
	    return this.getView().getSelection()[0]?.data.store;
	},

	getData: function() {
	    let view = this.getView();
	    let datastore = this.getDatastoreName();
	    return view.getStore().getById(datastore).data;
	},

	editGCJob: function() {
	    let data = this.getData();
	    Ext.create('PBS.window.GCJobEdit', {
		datastore: data.store,
		id: data.store,
		schedule: data.schedule,
		listeners: {
		    destroy: () => this.reload(),
		},
	    }).show();
	},

	garbageCollect: function() {
	    let datastore = this.getDatastoreName();
	    Proxmox.Utils.API2Request({
		url: `/admin/datastore/${datastore}/gc`,
		method: 'POST',
		failure: function(response) {
		    Ext.Msg.alert(gettext('Error'), response.htmlStatus);
		},
		success: function(response, options) {
		    Ext.create('Proxmox.window.TaskViewer', {
			upid: response.result.data,
		    }).show();
		},
	    });
	},

	showTaskLog: function() {
	    let me = this;

	    let upid = this.getData()['last-run-upid'];
	    if (!upid) return;

	    Ext.create('Proxmox.window.TaskViewer', { upid }).show();
	},

	startStore: function() { this.getView().getStore().rstore.startUpdate(); },
	stopStore: function() { this.getView().getStore().rstore.stopUpdate(); },
	reload: function() { this.getView().getStore().rstore.load(); },

    },

    listeners: {
	activate: 'startStore',
	destroy: 'stopStore',
	deactivate: 'stopStore',
	itemdblclick: 'editGCJob',
    },

    store: {
	type: 'diff',
	autoDestroy: true,
	autoDestroyRstore: true,
	sorters: 'store',
	rstore: {
	    type: 'update',
	    storeid: 'pbs-gc-jobs-status',
	    model: 'pbs-gc-jobs-status',
	    interval: 5000,
	},
    },

    tbar: [
	{
	    xtype: 'proxmoxButton',
	    text: gettext('Edit'),
	    handler: 'editGCJob',
	    enableFn: (rec) => !!rec,
	    disabled: true,
	},
	'-',
	{
	    xtype: 'proxmoxButton',
	    text: gettext('Show Log'),
	    handler: 'showTaskLog',
	    enableFn: (rec) => !!rec.data["last-run-upid"],
	    disabled: true,
	},
	{
	    xtype: 'proxmoxButton',
	    text: gettext('Run now'),
	    handler: 'garbageCollect',
	    enableFn: (rec) => !!rec,
	    disabled: true,
	},
    ],

    columns: [
	{
	    header: gettext('Datastore'),
	    dataIndex: 'store',
	    renderer: Ext.String.htmlEncode,
	    width: 120,
	    sortable: true,
	    hideable: false,
	},
	{
	    header: gettext('Schedule'),
	    dataIndex: 'schedule',
	    maxWidth: 220,
	    minWidth: 80,
	    flex: 1,
	    sortable: false,
	    hideable: false,
	    renderer: (value) => value ? value : Proxmox.Utils.NoneText,
	},
	{
	    header: gettext('Last GC'),
	    dataIndex: 'last-run-endtime',
	    renderer: PBS.Utils.render_optional_timestamp,
	    minWidth: 150,
	    sortable: true,
	},
	{
	    text: gettext('Duration'),
	    dataIndex: 'duration',
	    renderer: Proxmox.Utils.render_duration,
	    sortable: false,
	    width: 80,
	},
	{
	    header: gettext('Last Status'),
	    dataIndex: 'last-run-state',
	    renderer: PBS.Utils.render_task_status,
	    sortable: true,
	    flex: 3,
	    maxWidth: 100,
	    minWidth: 80,
	},
	{
	    header: gettext('Next Run'),
	    dataIndex: 'next-run',
	    renderer: PBS.Utils.render_next_task_run,
	    width: 150,
	    sortable: true,
	},
    ],
});
